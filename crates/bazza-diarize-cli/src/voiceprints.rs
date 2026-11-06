//! Native I/O adapter for bounded, label-free query evidence.
//! Valid primary vectors retain the existing normalizer's exact output bits.

use bazza_diarize::voice_evidence::{
    recover_embedding, ClusterEvidence, FiniteEmbedding, InvalidWindowReason as Reason,
    PrimaryResult, QuerySource, RecoveryEvidence, RecoveryPolicy, RecoveryWindow, SampleInterval,
    WindowEvidence, WindowPlan, WindowResult, MAX_COHERENCE_WINDOWS, MAX_EMBEDDING_DIM,
    MAX_POOL_SAMPLES, MAX_SOURCE_INTERVALS, SAMPLE_RATE,
};

pub struct ClusterExtraction {
    pub embedding: Option<Vec<f32>>,
    pub evidence: ClusterEvidence,
}

fn invalid(reason: Reason) -> WindowResult {
    WindowResult::Invalid { reason }
}

fn evaluate(
    audio: &[f32],
    dimension: usize,
    embed: &mut impl FnMut(&[f32]) -> Result<Vec<f32>, String>,
) -> WindowResult {
    if dimension == 0 || dimension > MAX_EMBEDDING_DIM {
        return invalid(Reason::WrongDimension);
    }
    if audio.len() < SAMPLE_RATE as usize {
        return invalid(Reason::TooShort);
    }
    if !audio.iter().all(|x| x.is_finite()) {
        return invalid(Reason::Nonfinite);
    }
    let raw = match embed(audio) {
        Ok(raw) => raw,
        Err(_) => return invalid(Reason::InferenceFailed),
    };
    if raw.len() != dimension {
        return invalid(Reason::WrongDimension);
    }
    if !raw.iter().all(|x| x.is_finite()) {
        return invalid(Reason::Nonfinite);
    }
    match crate::checked_normalized_embedding(&raw, dimension)
        .and_then(|v| FiniteEmbedding::new(v).ok())
    {
        Some(embedding) => WindowResult::Valid { embedding },
        None => invalid(Reason::ZeroNorm),
    }
}

fn valid_mapping(sources: &[SampleInterval], pool_samples: u64, audio_samples: u64) -> bool {
    if sources.is_empty() || sources.len() > MAX_SOURCE_INTERVALS {
        return false;
    }
    let (mut total, mut previous_end) = (0u64, 0u64);
    for source in sources {
        let Ok(n) = source.sample_count() else {
            return false;
        };
        if source.start_sample < previous_end || source.end_sample > audio_samples {
            return false;
        }
        let Some(sum) = total.checked_add(n) else {
            return false;
        };
        total = sum;
        previous_end = source.end_sample;
    }
    total == pool_samples
}

fn mapped_spans(sources: &[SampleInterval], part: SampleInterval) -> Vec<SampleInterval> {
    let mut offset = 0;
    let mut result = Vec::new();
    for source in sources {
        let end = offset + (source.end_sample - source.start_sample);
        let (a, b) = (part.start_sample.max(offset), part.end_sample.min(end));
        if b > a {
            result.push(SampleInterval {
                start_sample: source.start_sample + (a - offset),
                end_sample: source.start_sample + (b - offset),
            });
        }
        offset = end;
    }
    result
}

/// Extract a primary/recovered query and every separately planned observation.
pub fn extract_cluster(
    pool: &[f32],
    pool_sources: &[SampleInterval],
    plan: &WindowPlan,
    audio_samples: u64,
    embedding_dim: usize,
    mut read: impl FnMut(SampleInterval) -> Result<Vec<f32>, String>,
    mut embed: impl FnMut(&[f32]) -> Result<Vec<f32>, String>,
) -> ClusterExtraction {
    let pool_samples = pool.len() as u64;
    let primary = if pool_samples > MAX_POOL_SAMPLES {
        invalid(Reason::AudioReadFailed)
    } else {
        evaluate(pool, embedding_dim, &mut embed)
    };
    let primary_vector = match &primary {
        WindowResult::Valid { embedding } => Some(embedding.as_slice()),
        WindowResult::Invalid { .. } => None,
    };
    let mut recovery = RecoveryEvidence {
        primary: match &primary {
            WindowResult::Valid { .. } => PrimaryResult::Valid,
            WindowResult::Invalid { reason } => PrimaryResult::Invalid { reason: *reason },
        },
        query_source: QuerySource::Unavailable,
        pool_samples,
        windows: Vec::new(),
    };
    if primary_vector.is_none()
        && (6 * SAMPLE_RATE..=MAX_POOL_SAMPLES).contains(&pool_samples)
        && valid_mapping(pool_sources, pool_samples, audio_samples)
    {
        let n = pool_samples.div_ceil(30 * SAMPLE_RATE).max(2);
        for i in 0..n {
            let part = SampleInterval {
                start_sample: i * pool_samples / n,
                end_sample: (i + 1) * pool_samples / n,
            };
            let result = if part.end_sample - part.start_sample < 3 * SAMPLE_RATE {
                invalid(Reason::TooShort)
            } else {
                evaluate(
                    &pool[part.start_sample as usize..part.end_sample as usize],
                    embedding_dim,
                    &mut embed,
                )
            };
            recovery.windows.push(RecoveryWindow {
                pool: part,
                source_spans: mapped_spans(pool_sources, part),
                result,
            });
        }
    }
    let policy = RecoveryPolicy {
        embedding_dim,
        minimum_pool_samples: 6 * SAMPLE_RATE,
        minimum_window_samples: 3 * SAMPLE_RATE,
        minimum_valid_windows: 2,
        minimum_valid_fraction: 1.0,
        require_all_windows_valid: true,
    };
    let embedding = recover_embedding(
        primary_vector,
        pool_samples,
        &recovery.windows,
        audio_samples,
        policy,
    )
    .ok()
    .and_then(|result| {
        recovery.query_source = result.query_source;
        result.embedding.map(FiniteEmbedding::into_vec)
    });
    let coherence_windows = if plan.selected.len() > MAX_COHERENCE_WINDOWS {
        // An impossible/malformed plan must remain insufficient to its validator.
        Vec::new()
    } else {
        plan.selected
            .iter()
            .map(|window| {
                let result = match window.source.sample_count() {
                    Ok(n) if window.source.end_sample <= audio_samples && n <= MAX_POOL_SAMPLES => {
                        match read(window.source) {
                            Ok(audio) if audio.len() as u64 == n => {
                                evaluate(&audio, embedding_dim, &mut embed)
                            }
                            _ => invalid(Reason::AudioReadFailed),
                        }
                    }
                    _ => invalid(Reason::AudioReadFailed),
                };
                WindowEvidence {
                    window: window.clone(),
                    result,
                }
            })
            .collect()
    };
    ClusterExtraction {
        embedding,
        evidence: ClusterEvidence {
            recovery,
            coherence_windows,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_diarize::voice_evidence::{plan_coherence, PlannerConfig, SourceInterval};

    fn span(a: u64, b: u64) -> SampleInterval {
        SampleInterval {
            start_sample: a,
            end_sample: b,
        }
    }
    fn plan(spans: &[SampleInterval], bound: u64) -> WindowPlan {
        let sources: Vec<_> = spans
            .iter()
            .enumerate()
            .map(|(i, s)| SourceInterval {
                turn_index: i as u32,
                span: *s,
            })
            .collect();
        plan_coherence(&sources, bound, PlannerConfig::default()).unwrap()
    }
    fn empty_plan() -> WindowPlan {
        let mut p = plan(&[span(0, 16_000)], 16_000);
        p.selected.clear();
        p
    }

    #[test]
    fn valid_primary_keeps_exact_legacy_bits_and_still_collects_later_evidence() {
        let mut calls = 0;
        let p = plan(&[span(200_000, 280_000)], 280_000);
        let result = extract_cluster(
            &vec![1.; 16_000],
            &[span(0, 16_000)],
            &p,
            280_000,
            2,
            |s| {
                assert_eq!(s, span(200_000, 280_000));
                Ok(vec![1.; 80_000])
            },
            |_| {
                calls += 1;
                Ok(vec![3., 4.])
            },
        );
        let old = crate::checked_normalized_embedding(&[3., 4.], 2).unwrap();
        assert_eq!(
            result
                .embedding
                .unwrap()
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>(),
            old.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
        assert_eq!(calls, 2);
        assert_eq!(result.evidence.recovery.query_source, QuerySource::Primary);
        assert!(result.evidence.recovery.windows.is_empty());
        assert_eq!(result.evidence.coherence_windows.len(), 1);
    }

    #[test]
    fn recovery_maps_a_balanced_partition_across_disjoint_original_turns() {
        let mut pool = vec![1.; 48_000];
        pool.extend(vec![2.; 48_000]);
        let result = extract_cluster(
            &pool,
            &[span(100, 64_100), span(200_000, 232_000)],
            &empty_plan(),
            232_000,
            2,
            |_| panic!("empty coherence plan"),
            |x| {
                Ok(if x.len() == 96_000 {
                    vec![f32::NAN; 2]
                } else if x[0] == 1. {
                    vec![3., 0.]
                } else {
                    vec![0., 4.]
                })
            },
        );
        assert_eq!(
            result.evidence.recovery.query_source,
            QuerySource::Recovered
        );
        assert_eq!(
            result.evidence.recovery.windows[0].source_spans,
            vec![span(100, 48_100)]
        );
        assert_eq!(
            result.evidence.recovery.windows[1].source_spans,
            vec![span(48_100, 64_100), span(200_000, 232_000)]
        );
        for value in result.embedding.unwrap() {
            assert!((value - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-7);
        }
    }

    #[test]
    fn a_failed_tile_abstains_and_keeps_complete_serializable_failure_evidence() {
        let mut calls = 0;
        let result = extract_cluster(
            &vec![1.; 96_000],
            &[span(0, 96_000)],
            &empty_plan(),
            96_000,
            2,
            |_| unreachable!(),
            |_| {
                calls += 1;
                match calls {
                    1 => Ok(vec![f32::NAN; 2]),
                    2 => Ok(vec![1., 0.]),
                    _ => Err("model failure".into()),
                }
            },
        );
        assert!(result.embedding.is_none());
        assert_eq!(calls, 3);
        assert_eq!(result.evidence.recovery.windows.len(), 2);
        assert_eq!(
            result.evidence.recovery.windows[1].result,
            invalid(Reason::InferenceFailed)
        );
        assert_eq!(
            result.evidence.recovery.query_source,
            QuerySource::Unavailable
        );
        let json = serde_json::to_string(&result.evidence).unwrap();
        assert_eq!(
            serde_json::from_str::<ClusterEvidence>(&json).unwrap(),
            result.evidence
        );
    }

    #[test]
    fn wrong_length_nonfinite_audio_and_model_failures_do_not_hide_planned_windows() {
        let p = plan(
            &[
                span(0, 80_000),
                span(100_000, 180_000),
                span(200_000, 280_000),
            ],
            280_000,
        );
        let mut calls = 0;
        let result = extract_cluster(
            &vec![1.; 16_000],
            &[span(0, 16_000)],
            &p,
            280_000,
            2,
            |s| {
                Ok(match s.start_sample {
                    0 => vec![1.; 79_999],
                    100_000 => vec![f32::NAN; 80_000],
                    _ => vec![1.; 80_000],
                })
            },
            |x| {
                calls += 1;
                if x.len() == 16_000 {
                    Ok(vec![3., 4.])
                } else {
                    Err("model failure".into())
                }
            },
        );
        assert!(result.embedding.is_some());
        assert_eq!(calls, 2);
        assert_eq!(
            result
                .evidence
                .coherence_windows
                .iter()
                .map(|w| w.result.clone())
                .collect::<Vec<_>>(),
            vec![
                invalid(Reason::AudioReadFailed),
                invalid(Reason::Nonfinite),
                invalid(Reason::InferenceFailed)
            ]
        );
    }

    #[test]
    fn overlapping_recovery_provenance_never_triggers_additional_model_calls() {
        let mut calls = 0;
        let result = extract_cluster(
            &vec![1.; 96_000],
            &[span(0, 64_000), span(32_000, 64_000)],
            &empty_plan(),
            96_000,
            2,
            |_| unreachable!(),
            |_| {
                calls += 1;
                Ok(vec![f32::NAN; 2])
            },
        );
        assert_eq!(calls, 1);
        assert!(result.embedding.is_none());
        assert!(result.evidence.recovery.windows.is_empty());
    }
}
