//! Conservative temporal contradiction checks, separate from identity acceptance.
//! Relative nearest-profile ranks never establish a person's identity. They may
//! veto an existing aggregate decision or reusable query when observations disagree.

use crate::identify::{best_match, nearest_profile, rankable_profile_count, KnownSpeaker};
use crate::voice_evidence::{
    plan_coherence, recover_embedding, validate_cluster, ClusterEvidence, EvidenceStatus,
    FiniteEmbedding, PlannerConfig, PrimaryResult, QuerySource, RecoveryPolicy, SampleInterval,
    SourceInterval, StratumRequirement, ValidationConfig, WindowResult, SAMPLE_RATE,
};

pub const POLICY_ID: &str = "titanet-pool30-window5-tail1-rank-veto-compat-v2";
pub const PRIMARY_POOL_LIMIT: u64 = 60 * SAMPLE_RATE;
pub const RECOVERY_TILE_LIMIT: u64 = 30 * SAMPLE_RATE;

pub fn planner_config() -> PlannerConfig {
    PlannerConfig {
        minimum_tail_samples: SAMPLE_RATE,
        ..PlannerConfig::default()
    }
}

pub fn recovery_policy(embedding_dim: usize) -> RecoveryPolicy {
    RecoveryPolicy {
        embedding_dim,
        minimum_pool_samples: 6 * SAMPLE_RATE,
        minimum_window_samples: 3 * SAMPLE_RATE,
        minimum_valid_windows: 2,
        minimum_valid_fraction: 1.0,
        require_all_windows_valid: true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceStatus {
    Eligible,
    CompatibilityOnly,
    Missing,
    Invalid,
    Insufficient,
    RankConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceAssessment {
    pub status: VoiceStatus,
    /// The result of the existing aggregate verifier, never of raw window ranks.
    pub verified_id: Option<String>,
}

impl VoiceAssessment {
    pub fn reusable(&self) -> bool {
        self.status == VoiceStatus::Eligible
    }
    /// Compatibility preserves only the pre-existing, independently verified
    /// aggregate proposal; it never certifies the cluster or teaches Voices.
    pub fn naming_allowed(&self) -> bool {
        matches!(
            self.status,
            VoiceStatus::Eligible | VoiceStatus::CompatibilityOnly
        )
    }
    pub fn missing() -> Self {
        Self {
            status: VoiceStatus::Missing,
            verified_id: None,
        }
    }
}

fn fail(status: VoiceStatus) -> VoiceAssessment {
    VoiceAssessment {
        status,
        verified_id: None,
    }
}

fn compatibility(evidence: &ClusterEvidence, accepted: Option<&KnownSpeaker>) -> VoiceAssessment {
    if evidence.recovery.query_source == QuerySource::Primary {
        VoiceAssessment {
            status: VoiceStatus::CompatibilityOnly,
            verified_id: accepted.map(|p| p.id.clone()),
        }
    } else {
        fail(VoiceStatus::Insufficient)
    }
}

/// Authoritative prefix source mapping, with no padding, repeated speech or gaps
/// counted as observed audio. Positions share the sidecar's complete input clock.
pub fn prefix_sources(sources: &[SourceInterval], cap: u64) -> Vec<SampleInterval> {
    let mut sorted = sources.to_vec();
    sorted.sort_by_key(|s| (s.span.start_sample, s.span.end_sample, s.turn_index));
    let mut remaining = cap;
    let mut result = Vec::new();
    for source in sorted {
        if remaining == 0 {
            break;
        }
        let Some(length) = source.span.end_sample.checked_sub(source.span.start_sample) else {
            return Vec::new();
        };
        let take = remaining.min(length);
        if take > 0 {
            result.push(SampleInterval {
                start_sample: source.span.start_sample,
                end_sample: source.span.start_sample + take,
            });
            remaining -= take;
        }
    }
    result
}

fn map_pool_interval(sources: &[SampleInterval], part: SampleInterval) -> Vec<SampleInterval> {
    let mut offset = 0;
    let mut mapped = Vec::new();
    for source in sources {
        let end = offset + source.end_sample - source.start_sample;
        let (a, b) = (part.start_sample.max(offset), part.end_sample.min(end));
        if b > a {
            mapped.push(SampleInterval {
                start_sample: source.start_sample + a - offset,
                end_sample: source.start_sample + b - offset,
            });
        }
        offset = end;
    }
    mapped
}

fn valid_recovery(
    embedding: &[f32],
    evidence: &ClusterEvidence,
    sources: &[SourceInterval],
    audio_samples: u64,
    dimension: usize,
) -> bool {
    if embedding.len() != dimension || FiniteEmbedding::new(embedding.to_vec()).is_err() {
        return false;
    }
    let norm2 = embedding.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
    if (norm2 - 1.0).abs() > 1e-4 {
        return false;
    }
    let mapped = prefix_sources(sources, PRIMARY_POOL_LIMIT);
    let total: u64 = mapped.iter().map(|s| s.end_sample - s.start_sample).sum();
    let recovery = &evidence.recovery;
    if total == 0 || recovery.pool_samples != total {
        return false;
    }
    match recovery.query_source {
        QuerySource::Primary => {
            matches!(recovery.primary, PrimaryResult::Valid) && recovery.windows.is_empty()
        }
        QuerySource::Unavailable => false,
        QuerySource::Recovered => {
            if !matches!(recovery.primary, PrimaryResult::Invalid { .. }) {
                return false;
            }
            let count = total.div_ceil(RECOVERY_TILE_LIMIT).max(2);
            if recovery.windows.len() as u64 != count {
                return false;
            }
            for (index, window) in recovery.windows.iter().enumerate() {
                let part = SampleInterval {
                    start_sample: index as u64 * total / count,
                    end_sample: (index as u64 + 1) * total / count,
                };
                if window.pool != part || window.source_spans != map_pool_interval(&mapped, part) {
                    return false;
                }
            }
            recover_embedding(
                None,
                total,
                &recovery.windows,
                audio_samples,
                recovery_policy(dimension),
            )
            .ok()
            .and_then(|r| r.embedding)
            .is_some_and(|r| {
                r.as_slice()
                    .iter()
                    .zip(embedding)
                    .all(|(a, b)| a.to_bits() == b.to_bits())
            })
        }
    }
}

/// Inspect all selected finite windows. No new cosine acceptance threshold is
/// fitted here. With one reference profile, relative rank alone is uninformative,
/// so every window must also pass the unchanged identity verifier before reuse.
/// Insufficient valid primary observations preserve the old aggregate suggestion
/// only; new recovered queries never use that compatibility exception.
pub fn assess_cluster(
    embedding: Option<&[f32]>,
    evidence: &ClusterEvidence,
    sources: &[SourceInterval],
    audio_samples: u64,
    dimension: usize,
    known: &[KnownSpeaker],
    threshold: f32,
) -> VoiceAssessment {
    if !threshold.is_finite() {
        return fail(VoiceStatus::Invalid);
    }
    let Ok(plan) = plan_coherence(sources, audio_samples, planner_config()) else {
        return fail(VoiceStatus::Invalid);
    };
    let geometry = validate_cluster(
        &evidence.coherence_windows,
        &plan,
        ValidationConfig {
            embedding_dim: dimension,
            minimum_valid_windows: 2,
            minimum_eligible_fraction: 0.0,
            require_all_selected_valid: true,
            stratum_requirement: StratumRequirement::AllEligibleOccupied,
            coherence_rule: None,
        },
    );
    if geometry.status == EvidenceStatus::Invalid {
        return fail(VoiceStatus::Invalid);
    }
    let Some(embedding) = embedding else {
        return fail(VoiceStatus::Insufficient);
    };
    if !valid_recovery(embedding, evidence, sources, audio_samples, dimension) {
        return fail(VoiceStatus::Invalid);
    }
    let accepted = best_match(embedding, known, threshold);
    if !evidence
        .coherence_windows
        .iter()
        .all(|w| matches!(w.result, WindowResult::Valid { .. }))
    {
        return fail(VoiceStatus::Insufficient);
    }
    let rankable = rankable_profile_count(embedding, known);
    if rankable == 0 {
        return compatibility(evidence, None);
    }
    // An unverified aggregate cannot supply an identity anchor. In that case,
    // consistency is relative to the first local observation; it still cannot
    // create a known-name proposal without the aggregate verifier above.
    let mut anchor = accepted.map(|(profile, _)| profile.id.as_str());
    let mut single_unverified = rankable == 1 && accepted.is_none();
    for observation in &evidence.coherence_windows {
        let WindowResult::Valid { embedding: window } = &observation.result else {
            return fail(VoiceStatus::Insufficient);
        };
        let Some((profile, _)) = nearest_profile(window.as_slice(), known) else {
            return fail(VoiceStatus::Insufficient);
        };
        if anchor.is_some_and(|id| profile.id != id) {
            return fail(VoiceStatus::RankConflict);
        }
        anchor = Some(profile.id.as_str());
        if rankable == 1 && best_match(window.as_slice(), known, threshold).is_none() {
            single_unverified = true;
        }
    }
    if single_unverified || geometry.status != EvidenceStatus::SufficientUnchecked {
        return compatibility(evidence, accepted.map(|(p, _)| p));
    }
    VoiceAssessment {
        status: VoiceStatus::Eligible,
        verified_id: accepted.map(|(p, _)| p.id.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_evidence::{plan_coherence, RecoveryEvidence, WindowEvidence};

    fn profile(id: &str, x: bool) -> KnownSpeaker {
        let samples = if x {
            vec![vec![1.0, 0.1], vec![1.0, -0.1]]
        } else {
            vec![vec![0.1, 1.0], vec![-0.1, 1.0]]
        };
        KnownSpeaker {
            id: id.into(),
            name: id.into(),
            embedding: crate::centroid(&samples),
            samples,
        }
    }
    fn fixture() -> (Vec<SourceInterval>, ClusterEvidence) {
        let sources = vec![SourceInterval {
            turn_index: 0,
            span: SampleInterval {
                start_sample: 0,
                end_sample: 10 * SAMPLE_RATE,
            },
        }];
        let plan = plan_coherence(&sources, 10 * SAMPLE_RATE, planner_config()).unwrap();
        let evidence = ClusterEvidence {
            recovery: RecoveryEvidence {
                primary: PrimaryResult::Valid,
                query_source: QuerySource::Primary,
                pool_samples: 10 * SAMPLE_RATE,
                windows: vec![],
            },
            coherence_windows: plan
                .selected
                .iter()
                .map(|w| WindowEvidence {
                    window: w.clone(),
                    result: WindowResult::Valid {
                        embedding: FiniteEmbedding::new(vec![1.0, 0.0]).unwrap(),
                    },
                })
                .collect(),
        };
        (sources, evidence)
    }
    fn check(
        evidence: &ClusterEvidence,
        sources: &[SourceInterval],
        known: &[KnownSpeaker],
        query: &[f32],
    ) -> VoiceAssessment {
        assess_cluster(
            Some(query),
            evidence,
            sources,
            10 * SAMPLE_RATE,
            2,
            known,
            0.62,
        )
    }

    #[test]
    fn consistent_ranks_only_preserve_a_separately_verified_identity() {
        let (s, e) = fixture();
        let known = vec![profile("A", true), profile("B", false)];
        assert_eq!(
            check(&e, &s, &known, &[1.0, 0.0]).verified_id.as_deref(),
            Some("A")
        );
        let unknown = check(&e, &s, &known, &[0.8, 0.6]);
        assert!(unknown.reusable());
        assert_eq!(unknown.verified_id, None);
    }
    #[test]
    fn one_contradictory_window_vetoes_a_dominant_accepted_average_and_reuse() {
        let (s, mut e) = fixture();
        let known = vec![profile("A", true), profile("B", false)];
        e.coherence_windows[1].result = WindowResult::Valid {
            embedding: FiniteEmbedding::new(vec![0.0, 1.0]).unwrap(),
        };
        assert_eq!(
            check(&e, &s, &known, &[1.0, 0.0]).status,
            VoiceStatus::RankConflict
        );
        assert!(!check(&e, &s, &known, &[1.0, 0.0]).reusable());
    }
    #[test]
    fn an_unverified_aggregate_cannot_override_consistent_local_ranks_or_create_a_name() {
        let (s, mut e) = fixture();
        let known = vec![profile("A", true), profile("B", false)];
        for window in &mut e.coherence_windows {
            window.result = WindowResult::Valid {
                embedding: FiniteEmbedding::new(vec![0.0, 1.0]).unwrap(),
            };
        }
        let checked = check(&e, &s, &known, &[0.8, 0.6]);
        assert!(checked.reusable());
        assert_eq!(checked.verified_id, None);
        let tied = std::f32::consts::FRAC_1_SQRT_2;
        let checked = check(&e, &s, &known, &[tied, tied]);
        assert!(checked.reusable());
        assert_eq!(checked.verified_id, None);
        e.coherence_windows[1].result = WindowResult::Valid {
            embedding: FiniteEmbedding::new(vec![1.0, 0.0]).unwrap(),
        };
        assert_eq!(
            check(&e, &s, &known, &[0.8, 0.6]).status,
            VoiceStatus::RankConflict
        );
    }
    #[test]
    fn unavailable_or_malformed_windows_cannot_pass_by_keeping_the_good_rows() {
        let (s, mut e) = fixture();
        let known = vec![profile("A", true), profile("B", false)];
        e.coherence_windows.pop();
        assert_eq!(
            check(&e, &s, &known, &[1.0, 0.0]).status,
            VoiceStatus::Invalid
        );
    }
    #[test]
    fn absent_reference_and_single_profile_do_not_turn_rank_into_proof() {
        let (s, e) = fixture();
        assert_eq!(
            check(&e, &s, &[], &[1.0, 0.0]).status,
            VoiceStatus::CompatibilityOnly
        );
        let only = vec![profile("A", true)];
        assert!(check(&e, &s, &only, &[1.0, 0.0]).reusable());
        let mut bad = e;
        bad.coherence_windows[1].result = WindowResult::Valid {
            embedding: FiniteEmbedding::new(vec![0.1, 1.0]).unwrap(),
        };
        assert_eq!(
            check(&bad, &s, &only, &[1.0, 0.0]).status,
            VoiceStatus::CompatibilityOnly
        );
        assert!(!check(&bad, &s, &only, &[1.0, 0.0]).reusable());
    }
    #[test]
    fn short_primary_compatibility_cannot_become_a_reusable_observation() {
        let (mut s, mut e) = fixture();
        s[0].span.end_sample = 3 * SAMPLE_RATE;
        let plan = plan_coherence(&s, 10 * SAMPLE_RATE, planner_config()).unwrap();
        e.coherence_windows = plan
            .selected
            .into_iter()
            .map(|window| WindowEvidence {
                window,
                result: WindowResult::Valid {
                    embedding: FiniteEmbedding::new(vec![1.0, 0.0]).unwrap(),
                },
            })
            .collect();
        e.recovery.pool_samples = 3 * SAMPLE_RATE;
        let known = vec![profile("A", true), profile("B", false)];
        let checked = check(&e, &s, &known, &[1.0, 0.0]);
        assert!(checked.naming_allowed());
        assert!(!checked.reusable());
        assert_eq!(checked.verified_id.as_deref(), Some("A"));
        e.coherence_windows[0].result = WindowResult::Valid {
            embedding: FiniteEmbedding::new(vec![0.0, 1.0]).unwrap(),
        };
        assert_eq!(
            check(&e, &s, &known, &[1.0, 0.0]).status,
            VoiceStatus::RankConflict,
            "even one observed contradiction must veto compatibility"
        );
    }
    #[test]
    fn one_profile_fallback_cannot_skip_a_later_uninformative_window() {
        let (s, mut e) = fixture();
        let only = vec![profile("A", true)];
        e.coherence_windows[0].result = WindowResult::Valid {
            embedding: FiniteEmbedding::new(vec![0.1, 1.0]).unwrap(),
        };
        e.coherence_windows[1].result = WindowResult::Valid {
            embedding: FiniteEmbedding::new(vec![-1.0, 0.0]).unwrap(),
        };
        assert!(!check(&e, &s, &only, &[1.0, 0.0]).naming_allowed());
    }
    #[test]
    fn prefix_mapping_never_counts_time_gaps_as_observed_speech() {
        let sources = vec![
            SourceInterval {
                turn_index: 0,
                span: SampleInterval {
                    start_sample: 0,
                    end_sample: 5,
                },
            },
            SourceInterval {
                turn_index: 1,
                span: SampleInterval {
                    start_sample: 20,
                    end_sample: 40,
                },
            },
        ];
        assert_eq!(
            prefix_sources(&sources, 12),
            vec![
                SampleInterval {
                    start_sample: 0,
                    end_sample: 5
                },
                SampleInterval {
                    start_sample: 20,
                    end_sample: 27
                }
            ]
        );
    }
}
