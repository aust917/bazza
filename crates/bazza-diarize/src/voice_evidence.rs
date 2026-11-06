//! Bounded query evidence, kept separate from independently enrolled voices.
//!
//! Sample positions are absolute, half-open intervals at 16 kHz. The planner
//! inspects the entire original cluster timeline; numerical recovery instead
//! describes a bounded concatenated prefix pool. Neither finite vectors nor
//! sampled consistency prove that uninspected speech belongs to one person.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};

pub const EVIDENCE_VERSION: u32 = 1;
/// Extraction geometry only; identity verification remains a separate policy.
pub const WINDOW_POLICY_ID: &str = "titanet-pool30-window5-tail3-strata4x4-v1";
pub const SAMPLE_RATE: u64 = 16_000;
pub const MAX_EMBEDDING_DIM: usize = 4096;
pub const MAX_SOURCE_INTERVALS: usize = 65_536;
pub const MAX_COHERENCE_WINDOWS: usize = 16;
pub const MAX_RECOVERY_WINDOWS: usize = 120;
pub const MAX_POOL_SAMPLES: u64 = 120 * SAMPLE_RATE;
pub const MAX_EVIDENCE_CLUSTERS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid voice evidence: {0}")]
pub struct EvidenceError(pub &'static str);

/// Validated finite, nonzero components. Construction never changes their bits.
#[derive(Debug, Clone, PartialEq)]
pub struct FiniteEmbedding(Vec<f32>);

impl FiniteEmbedding {
    pub fn new(values: Vec<f32>) -> Result<Self, EvidenceError> {
        if values.is_empty() || values.len() > MAX_EMBEDDING_DIM {
            return Err(EvidenceError("embedding dimension out of bounds"));
        }
        if !values.iter().all(|v| v.is_finite()) {
            return Err(EvidenceError("nonfinite embedding"));
        }
        let norm2 = values.iter().map(|v| (*v as f64).powi(2)).sum::<f64>();
        if !norm2.is_finite() || norm2 <= 0.0 {
            return Err(EvidenceError("zero or invalid embedding norm"));
        }
        Ok(Self(values))
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    pub fn into_vec(self) -> Vec<f32> {
        self.0
    }
}

impl Serialize for FiniteEmbedding {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for FiniteEmbedding {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(Vec::<f32>::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SampleInterval {
    pub start_sample: u64,
    pub end_sample: u64,
}

impl SampleInterval {
    pub fn sample_count(self) -> Result<u64, EvidenceError> {
        self.end_sample
            .checked_sub(self.start_sample)
            .filter(|n| *n > 0)
            .ok_or(EvidenceError("empty or reversed interval"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInterval {
    pub turn_index: u32,
    pub span: SampleInterval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId {
    pub turn_index: u32,
    pub tile_index: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedWindow {
    pub id: WindowId,
    pub source: SampleInterval,
    pub source_clock_stratum: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidWindowReason {
    Nonfinite,
    ZeroNorm,
    WrongDimension,
    TooShort,
    InferenceFailed,
    AudioReadFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WindowResult {
    Valid { embedding: FiniteEmbedding },
    Invalid { reason: InvalidWindowReason },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowEvidence {
    pub window: PlannedWindow,
    pub result: WindowResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuerySource {
    Primary,
    Recovered,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PrimaryResult {
    Valid,
    Invalid { reason: InvalidWindowReason },
    NotAttempted,
}

/// Pool coordinates are distinct from the absolute source coordinates below.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryWindow {
    pub pool: SampleInterval,
    pub source_spans: Vec<SampleInterval>,
    pub result: WindowResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecoveryEvidence {
    pub primary: PrimaryResult,
    pub query_source: QuerySource,
    pub pool_samples: u64,
    pub windows: Vec<RecoveryWindow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClusterEvidence {
    pub recovery: RecoveryEvidence,
    pub coherence_windows: Vec<WindowEvidence>,
}

/// Map keys always refer to original native cluster IDs, before consolidation.
/// Add this as an optional serde-defaulted field to the sidecar output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoiceEvidence {
    pub version: u32,
    pub model_id: String,
    pub model_sha256: String,
    pub policy_id: String,
    pub clusters: BTreeMap<i32, ClusterEvidence>,
}

pub fn validate_envelope(
    evidence: &VoiceEvidence,
    model_id: &str,
    model_sha256: &str,
    policy_id: &str,
) -> Result<(), EvidenceError> {
    if evidence.version != EVIDENCE_VERSION {
        return Err(EvidenceError("unsupported version"));
    }
    if evidence.model_id.is_empty()
        || evidence.model_id.len() > 128
        || evidence.policy_id.is_empty()
        || evidence.policy_id.len() > 128
        || evidence.model_sha256.len() != 64
        || !evidence.model_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || evidence.model_id != model_id
        || evidence.policy_id != policy_id
        || !evidence.model_sha256.eq_ignore_ascii_case(model_sha256)
    {
        return Err(EvidenceError("model or policy identity mismatch"));
    }
    if evidence.clusters.len() > MAX_EVIDENCE_CLUSTERS {
        return Err(EvidenceError("too many original clusters"));
    }
    for (id, cluster) in &evidence.clusters {
        if *id < 0 {
            return Err(EvidenceError("negative original cluster ID"));
        }
        if cluster.coherence_windows.len() > MAX_COHERENCE_WINDOWS
            || cluster.recovery.windows.len() > MAX_RECOVERY_WINDOWS
            || cluster.recovery.pool_samples > MAX_POOL_SAMPLES
        {
            return Err(EvidenceError("window or pool bound exceeded"));
        }
        if matches!(cluster.recovery.primary, PrimaryResult::Valid)
            != (cluster.recovery.query_source == QuerySource::Primary)
            || (cluster.recovery.query_source == QuerySource::Recovered
                && cluster.recovery.windows.is_empty())
        {
            return Err(EvidenceError("inconsistent recovery descriptor"));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerConfig {
    pub tile_samples: u64,
    pub minimum_tail_samples: u64,
    pub windows_per_stratum: usize,
}

impl Default for PlannerConfig {
    fn default() -> Self {
        Self {
            tile_samples: 5 * SAMPLE_RATE,
            minimum_tail_samples: 3 * SAMPLE_RATE,
            windows_per_stratum: 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPlan {
    pub candidates: Vec<PlannedWindow>,
    pub selected: Vec<PlannedWindow>,
    pub unsupported: Vec<SourceInterval>,
    pub speech_occupied_strata: BTreeSet<u8>,
    pub eligible_occupied_strata: BTreeSet<u8>,
    pub source_samples: u64,
    pub eligible_samples: u64,
    pub source_clock_start_sample: u64,
    pub source_clock_end_sample: u64,
}

fn bounded_span(span: SampleInterval, bound: u64) -> Result<u64, EvidenceError> {
    let n = span.sample_count()?;
    if span.end_sample > bound {
        return Err(EvidenceError("interval exceeds audio bounds"));
    }
    Ok(n)
}

fn canonical_sources(
    spans: &[SourceInterval],
    audio_samples: u64,
) -> Result<Vec<SourceInterval>, EvidenceError> {
    if spans.is_empty() || spans.len() > MAX_SOURCE_INTERVALS {
        return Err(EvidenceError("source interval count out of bounds"));
    }
    let mut ids = BTreeSet::new();
    let mut sorted = spans.to_vec();
    for source in &sorted {
        bounded_span(source.span, audio_samples)?;
        if !ids.insert(source.turn_index) {
            return Err(EvidenceError("duplicate source turn ID"));
        }
    }
    sorted.sort_by_key(|s| (s.span.start_sample, s.span.end_sample, s.turn_index));
    if sorted
        .windows(2)
        .any(|pair| pair[0].span.end_sample > pair[1].span.start_sample)
    {
        return Err(EvidenceError("overlapping or duplicate source intervals"));
    }
    Ok(sorted)
}

fn round_ratio_even(numerator: u128, denominator: u128) -> u128 {
    let (q, r) = (numerator / denominator, numerator % denominator);
    q + u128::from(2 * r > denominator || (2 * r == denominator && q % 2 == 1))
}

/// Exact integer-sample counterpart of the frozen Python planner. No bridging
/// across turns or label-dependent selection; candidate endpoints are retained.
pub fn plan_coherence(
    sources: &[SourceInterval],
    audio_samples: u64,
    config: PlannerConfig,
) -> Result<WindowPlan, EvidenceError> {
    if config.minimum_tail_samples == 0
        || config.tile_samples < config.minimum_tail_samples
        || config.windows_per_stratum == 0
        || config.windows_per_stratum > 4
    {
        return Err(EvidenceError("invalid planner configuration"));
    }
    let sources = canonical_sources(sources, audio_samples)?;
    let first = sources.first().expect("nonempty checked").span.start_sample;
    let last = sources.last().expect("nonempty checked").span.end_sample;
    let clock_span = u128::from(last - first);
    let mut plan = WindowPlan {
        candidates: Vec::new(),
        selected: Vec::new(),
        unsupported: Vec::new(),
        speech_occupied_strata: BTreeSet::new(),
        eligible_occupied_strata: BTreeSet::new(),
        source_samples: 0,
        eligible_samples: 0,
        source_clock_start_sample: first,
        source_clock_end_sample: last,
    };
    // One million candidates is a fixed work bound, independent of forged WAV duration.
    const MAX_CANDIDATES: usize = 1_000_000;
    for source in sources {
        let (mut a, b) = (source.span.start_sample, source.span.end_sample);
        plan.source_samples = plan
            .source_samples
            .checked_add(b - a)
            .ok_or(EvidenceError("sample sum overflow"))?;
        for q in 0..4u8 {
            let lower = first + (clock_span * u128::from(q) / 4) as u64;
            let upper = first + ((clock_span * u128::from(q + 1)).div_ceil(4)) as u64;
            if b.min(upper) > a.max(lower) {
                plan.speech_occupied_strata.insert(q);
            }
        }
        let mut tile = 0u32;
        while b - a >= config.minimum_tail_samples {
            if plan.candidates.len() >= MAX_CANDIDATES {
                return Err(EvidenceError("candidate window bound exceeded"));
            }
            let end = a.saturating_add(config.tile_samples).min(b);
            let midpoint_offset_twice = u128::from(a - first) + u128::from(end - first);
            let stratum = (4 * midpoint_offset_twice / (2 * clock_span)).min(3) as u8;
            plan.candidates.push(PlannedWindow {
                id: WindowId {
                    turn_index: source.turn_index,
                    tile_index: tile,
                },
                source: SampleInterval {
                    start_sample: a,
                    end_sample: end,
                },
                source_clock_stratum: stratum,
            });
            plan.eligible_occupied_strata.insert(stratum);
            plan.eligible_samples += end - a;
            a = end;
            tile = tile
                .checked_add(1)
                .ok_or(EvidenceError("tile index overflow"))?;
        }
        if a < b {
            plan.unsupported.push(SourceInterval {
                turn_index: source.turn_index,
                span: SampleInterval {
                    start_sample: a,
                    end_sample: b,
                },
            });
        }
    }
    for stratum in 0..4u8 {
        let rows: Vec<_> = plan
            .candidates
            .iter()
            .filter(|w| w.source_clock_stratum == stratum)
            .collect();
        let k = rows.len().min(config.windows_per_stratum);
        if k == 1 {
            plan.selected.push(rows[0].clone());
        } else if k > 1 {
            for i in 0..k {
                let index =
                    round_ratio_even((i * (rows.len() - 1)) as u128, (k - 1) as u128) as usize;
                plan.selected.push(rows[index].clone());
            }
        }
    }
    plan.selected
        .sort_by_key(|w| (w.source.start_sample, w.source.end_sample, w.id.turn_index));
    Ok(plan)
}

/// Union length is reported independently of summed probe durations. Adjacent
/// spans are harmless; duplicate/overlapping evidence is rejected by validators.
pub fn unique_sample_coverage(spans: &[SampleInterval]) -> Result<u64, EvidenceError> {
    if spans.len() > MAX_SOURCE_INTERVALS {
        return Err(EvidenceError("coverage interval bound exceeded"));
    }
    let mut spans = spans.to_vec();
    for span in &spans {
        span.sample_count()?;
    }
    spans.sort_unstable();
    let mut covered = 0u64;
    let mut current: Option<SampleInterval> = None;
    for span in spans {
        match current {
            Some(mut prior) if span.start_sample <= prior.end_sample => {
                prior.end_sample = prior.end_sample.max(span.end_sample);
                current = Some(prior);
            }
            Some(prior) => {
                covered = covered
                    .checked_add(prior.sample_count()?)
                    .ok_or(EvidenceError("coverage overflow"))?;
                current = Some(span);
            }
            None => current = Some(span),
        }
    }
    if let Some(prior) = current {
        covered = covered
            .checked_add(prior.sample_count()?)
            .ok_or(EvidenceError("coverage overflow"))?;
    }
    Ok(covered)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceStatus {
    Invalid,
    Insufficient,
    SufficientUnchecked,
    ConsistentSampled,
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoherenceStatistic {
    Minimum,
    P10,
}

/// These are different explicit policies, never an implicit relaxation when
/// short speech occupies a clock region with no model-eligible window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StratumRequirement {
    None,
    AllSpeechOccupied,
    AllEligibleOccupied,
}

#[derive(Debug, Clone, Copy)]
pub struct CoherenceRule {
    pub statistic: CoherenceStatistic,
    pub minimum_cosine: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct ValidationConfig {
    pub embedding_dim: usize,
    pub minimum_valid_windows: usize,
    pub minimum_eligible_fraction: f64,
    pub require_all_selected_valid: bool,
    pub stratum_requirement: StratumRequirement,
    /// No deployment threshold is selected by this module.
    pub coherence_rule: Option<CoherenceRule>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CoverageMetrics {
    pub source_samples: u64,
    pub eligible_samples: u64,
    pub unsupported_source_samples: u64,
    pub selected_unique_samples: u64,
    pub valid_unique_samples: u64,
    pub selected_windows: usize,
    pub valid_windows: usize,
    pub occupied_strata: BTreeSet<u8>,
    pub eligible_occupied_strata: BTreeSet<u8>,
    pub valid_strata: BTreeSet<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairwiseMetrics {
    pub minimum: f64,
    pub p10: f64,
    pub pairs: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceAssessment {
    pub status: EvidenceStatus,
    pub reason: Option<String>,
    pub coverage: CoverageMetrics,
    pub pairwise: Option<PairwiseMetrics>,
}

fn pairwise_metrics(embeddings: &[&FiniteEmbedding]) -> Option<PairwiseMetrics> {
    let mut pairs = Vec::new();
    for (i, a) in embeddings.iter().enumerate() {
        for b in &embeddings[i + 1..] {
            let (mut dot, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
            for (x, y) in a.as_slice().iter().zip(b.as_slice()) {
                let (x, y) = (f64::from(*x), f64::from(*y));
                dot += x * y;
                aa += x * x;
                bb += y * y;
            }
            pairs.push((dot / (aa.sqrt() * bb.sqrt())).clamp(-1.0, 1.0));
        }
    }
    if pairs.is_empty() {
        return None;
    }
    pairs.sort_by(f64::total_cmp);
    let h = 0.1 * (pairs.len() - 1) as f64;
    let lower = h.floor() as usize;
    let upper = h.ceil() as usize;
    Some(PairwiseMetrics {
        minimum: pairs[0],
        p10: pairs[lower] + (h - lower as f64) * (pairs[upper] - pairs[lower]),
        pairs: pairs.len(),
    })
}

/// Validate against an authoritative plan, never against evidence's own claimed
/// denominator. Invalid/short scheduled windows remain visible and can abstain.
pub fn validate_cluster(
    windows: &[WindowEvidence],
    plan: &WindowPlan,
    config: ValidationConfig,
) -> EvidenceAssessment {
    let mut assessment = EvidenceAssessment {
        status: EvidenceStatus::Invalid,
        reason: None,
        coverage: CoverageMetrics {
            source_samples: plan.source_samples,
            eligible_samples: plan.eligible_samples,
            unsupported_source_samples: plan.source_samples.saturating_sub(plan.eligible_samples),
            selected_windows: plan.selected.len(),
            occupied_strata: plan.speech_occupied_strata.clone(),
            eligible_occupied_strata: plan.eligible_occupied_strata.clone(),
            ..Default::default()
        },
        pairwise: None,
    };
    let checked = (|| -> Result<(), EvidenceError> {
        if config.embedding_dim == 0
            || config.embedding_dim > MAX_EMBEDDING_DIM
            || config.minimum_valid_windows < 2
            || config.minimum_valid_windows > MAX_COHERENCE_WINDOWS
            || !config.minimum_eligible_fraction.is_finite()
            || !(0.0..=1.0).contains(&config.minimum_eligible_fraction)
            || config.coherence_rule.is_some_and(|r| {
                !r.minimum_cosine.is_finite() || !(-1.0..=1.0).contains(&r.minimum_cosine)
            })
        {
            return Err(EvidenceError("invalid validation configuration"));
        }
        if windows.len() > MAX_COHERENCE_WINDOWS
            || plan.selected.len() > MAX_COHERENCE_WINDOWS
            || windows.len() != plan.selected.len()
            || plan.eligible_samples > plan.source_samples
        {
            return Err(EvidenceError(
                "missing, excessive or inconsistent planned windows",
            ));
        }
        let expected: BTreeMap<_, _> = plan.selected.iter().map(|w| (w.id, w)).collect();
        if expected.len() != plan.selected.len() {
            return Err(EvidenceError("duplicate planned window ID"));
        }
        let mut seen = BTreeSet::new();
        let mut selected = Vec::new();
        let mut valid = Vec::new();
        let mut embeddings = Vec::new();
        for row in windows {
            if !seen.insert(row.window.id)
                || expected.get(&row.window.id).copied() != Some(&row.window)
            {
                return Err(EvidenceError("duplicate or altered planned window"));
            }
            row.window.source.sample_count()?;
            selected.push(row.window.source);
            if let WindowResult::Valid { embedding } = &row.result {
                if embedding.as_slice().len() != config.embedding_dim {
                    return Err(EvidenceError("window embedding dimension mismatch"));
                }
                valid.push(row.window.source);
                embeddings.push(embedding);
                assessment
                    .coverage
                    .valid_strata
                    .insert(row.window.source_clock_stratum);
            }
        }
        let summed = selected
            .iter()
            .try_fold(0u64, |sum, s| {
                sum.checked_add(s.end_sample - s.start_sample)
            })
            .ok_or(EvidenceError("selected sample sum overflow"))?;
        assessment.coverage.selected_unique_samples = unique_sample_coverage(&selected)?;
        if summed != assessment.coverage.selected_unique_samples {
            return Err(EvidenceError("overlapping selected windows"));
        }
        assessment.coverage.valid_unique_samples = unique_sample_coverage(&valid)?;
        assessment.coverage.valid_windows = valid.len();
        assessment.pairwise = pairwise_metrics(&embeddings);
        Ok(())
    })();
    if let Err(error) = checked {
        assessment.reason = Some(error.to_string());
        return assessment;
    }
    let coverage = &assessment.coverage;
    if coverage.source_samples == 0
        || coverage.valid_windows < config.minimum_valid_windows
        || (config.require_all_selected_valid
            && coverage.valid_windows != coverage.selected_windows)
        || (coverage.eligible_samples as f64)
            < config.minimum_eligible_fraction * coverage.source_samples as f64
        || match config.stratum_requirement {
            StratumRequirement::None => false,
            StratumRequirement::AllSpeechOccupied => {
                coverage.valid_strata != coverage.occupied_strata
            }
            StratumRequirement::AllEligibleOccupied => {
                coverage.valid_strata != coverage.eligible_occupied_strata
            }
        }
    {
        assessment.status = EvidenceStatus::Insufficient;
        assessment.reason = Some("insufficient valid sampled coverage".into());
    } else if let Some(rule) = config.coherence_rule {
        let metrics = assessment.pairwise.expect("at least two validated windows");
        let score = match rule.statistic {
            CoherenceStatistic::Minimum => metrics.minimum,
            CoherenceStatistic::P10 => metrics.p10,
        };
        assessment.status = if score < rule.minimum_cosine {
            EvidenceStatus::Conflict
        } else {
            EvidenceStatus::ConsistentSampled
        };
    } else {
        assessment.status = EvidenceStatus::SufficientUnchecked;
    }
    assessment
}

#[derive(Debug, Clone, Copy)]
pub struct RecoveryPolicy {
    pub embedding_dim: usize,
    pub minimum_pool_samples: u64,
    pub minimum_window_samples: u64,
    pub minimum_valid_windows: usize,
    pub minimum_valid_fraction: f64,
    pub require_all_windows_valid: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveryResult {
    pub embedding: Option<FiniteEmbedding>,
    pub query_source: QuerySource,
    pub valid_windows: usize,
    pub valid_samples: u64,
}

/// Preserve a valid primary vector exactly. Only invalid primaries use the
/// chronological finite tiles, with sample-weighted unit vectors and an f64 sum.
pub fn recover_embedding(
    primary: Option<&[f32]>,
    pool_samples: u64,
    windows: &[RecoveryWindow],
    audio_samples: u64,
    policy: RecoveryPolicy,
) -> Result<RecoveryResult, EvidenceError> {
    if policy.embedding_dim == 0
        || policy.embedding_dim > MAX_EMBEDDING_DIM
        || pool_samples == 0
        || pool_samples > MAX_POOL_SAMPLES
        || policy.minimum_pool_samples == 0
        || policy.minimum_pool_samples > MAX_POOL_SAMPLES
        || policy.minimum_window_samples == 0
        || policy.minimum_window_samples > policy.minimum_pool_samples
        || policy.minimum_valid_windows == 0
        || policy.minimum_valid_windows > MAX_RECOVERY_WINDOWS
        || !policy.minimum_valid_fraction.is_finite()
        || !(0.0..=1.0).contains(&policy.minimum_valid_fraction)
    {
        return Err(EvidenceError("invalid recovery configuration"));
    }
    if let Some(primary) = primary.filter(|p| p.len() == policy.embedding_dim) {
        if let Ok(embedding) = FiniteEmbedding::new(primary.to_vec()) {
            return Ok(RecoveryResult {
                embedding: Some(embedding),
                query_source: QuerySource::Primary,
                valid_windows: 0,
                valid_samples: 0,
            });
        }
    }
    if windows.len() > MAX_RECOVERY_WINDOWS {
        return Err(EvidenceError("too many recovery windows"));
    }
    let mut ordered: Vec<_> = windows.iter().collect();
    ordered.sort_by_key(|w| w.pool);
    let mut last = 0;
    let mut last_source_end = None;
    let mut short_window = false;
    let mut valid_windows = 0;
    let mut valid_samples = 0u64;
    let mut source_spans = Vec::new();
    let mut sum = vec![0.0f64; policy.embedding_dim];
    for window in ordered {
        let duration = bounded_span(window.pool, pool_samples)?;
        if window.pool.start_sample != last {
            return Err(EvidenceError(
                "recovery windows do not form a complete chronological partition",
            ));
        }
        last = window.pool.end_sample;
        short_window |= duration < policy.minimum_window_samples;
        if window.source_spans.is_empty() || window.source_spans.len() > MAX_SOURCE_INTERVALS {
            return Err(EvidenceError(
                "recovery source interval count out of bounds",
            ));
        }
        let mut mapped = 0u64;
        for span in &window.source_spans {
            if source_spans.len() >= MAX_SOURCE_INTERVALS {
                return Err(EvidenceError("total recovery mapping bound exceeded"));
            }
            mapped = mapped
                .checked_add(bounded_span(*span, audio_samples)?)
                .ok_or(EvidenceError("recovery mapping overflow"))?;
            if last_source_end.is_some_and(|end| span.start_sample < end) {
                return Err(EvidenceError(
                    "recovery source mapping is not chronological and disjoint",
                ));
            }
            last_source_end = Some(span.end_sample);
            source_spans.push(*span);
        }
        if mapped != duration {
            return Err(EvidenceError("recovery source mapping length mismatch"));
        }
        if let WindowResult::Valid { embedding } = &window.result {
            if embedding.as_slice().len() != policy.embedding_dim {
                return Err(EvidenceError("recovery dimension mismatch"));
            }
            let norm = embedding
                .as_slice()
                .iter()
                .map(|v| f64::from(*v).powi(2))
                .sum::<f64>()
                .sqrt();
            for (acc, value) in sum.iter_mut().zip(embedding.as_slice()) {
                *acc += f64::from(*value) / norm * duration as f64;
            }
            valid_windows += 1;
            valid_samples += duration;
        }
    }
    if !windows.is_empty() && last != pool_samples {
        return Err(EvidenceError(
            "recovery partition does not cover the declared pool",
        ));
    }
    let mapped_sum = source_spans
        .iter()
        .try_fold(0u64, |sum, s| {
            sum.checked_add(s.end_sample - s.start_sample)
        })
        .ok_or(EvidenceError("total recovery mapping overflow"))?;
    if unique_sample_coverage(&source_spans)? != mapped_sum {
        return Err(EvidenceError(
            "duplicate or overlapping recovery source samples",
        ));
    }
    let unavailable = || RecoveryResult {
        embedding: None,
        query_source: QuerySource::Unavailable,
        valid_windows,
        valid_samples,
    };
    if pool_samples < policy.minimum_pool_samples
        || short_window
        || valid_windows < policy.minimum_valid_windows
        || (policy.require_all_windows_valid && valid_windows != windows.len())
        || (valid_samples as f64) < policy.minimum_valid_fraction * pool_samples as f64
    {
        return Ok(unavailable());
    }
    let norm = sum.iter().map(|v| v * v).sum::<f64>().sqrt();
    if !norm.is_finite() || norm <= 0.0 {
        return Ok(unavailable());
    }
    let embedding = FiniteEmbedding::new(sum.iter().map(|v| (v / norm) as f32).collect())?;
    Ok(RecoveryResult {
        embedding: Some(embedding),
        query_source: QuerySource::Recovered,
        valid_windows,
        valid_samples,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start: u64, end: u64) -> SampleInterval {
        SampleInterval {
            start_sample: start,
            end_sample: end,
        }
    }
    fn source(index: u32, start: u64, end: u64) -> SourceInterval {
        SourceInterval {
            turn_index: index,
            span: span(start, end),
        }
    }
    fn embedding(v: &[f32]) -> FiniteEmbedding {
        FiniteEmbedding::new(v.to_vec()).unwrap()
    }
    fn valid(v: &[f32]) -> WindowResult {
        WindowResult::Valid {
            embedding: embedding(v),
        }
    }
    fn invalid() -> WindowResult {
        WindowResult::Invalid {
            reason: InvalidWindowReason::Nonfinite,
        }
    }
    fn config() -> ValidationConfig {
        ValidationConfig {
            embedding_dim: 2,
            minimum_valid_windows: 2,
            minimum_eligible_fraction: 0.5,
            require_all_selected_valid: true,
            stratum_requirement: StratumRequirement::AllSpeechOccupied,
            coherence_rule: None,
        }
    }
    fn rows(plan: &WindowPlan) -> Vec<WindowEvidence> {
        plan.selected
            .iter()
            .map(|w| WindowEvidence {
                window: w.clone(),
                result: valid(&[1.0, 0.0]),
            })
            .collect()
    }
    fn recovery_policy() -> RecoveryPolicy {
        RecoveryPolicy {
            embedding_dim: 2,
            minimum_pool_samples: 6 * SAMPLE_RATE,
            minimum_window_samples: 3 * SAMPLE_RATE,
            minimum_valid_windows: 2,
            minimum_valid_fraction: 1.0,
            require_all_windows_valid: true,
        }
    }
    fn tiles() -> Vec<RecoveryWindow> {
        vec![
            RecoveryWindow {
                pool: span(0, 480_000),
                source_spans: vec![span(100, 480_100)],
                result: valid(&[1.0, 0.0]),
            },
            RecoveryWindow {
                pool: span(480_000, 960_000),
                source_spans: vec![span(500_000, 980_000)],
                result: valid(&[1.0, 0.0]),
            },
        ]
    }

    #[test]
    fn finite_constructor_preserves_bits_and_handles_extreme_finite_norms() {
        for v in [
            vec![f32::MAX, 1.0],
            vec![f32::from_bits(1), 0.0],
            vec![0.25, -0.5],
        ] {
            let e = embedding(&v);
            assert_eq!(
                e.as_slice().iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
                v.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn invalid_vectors_cannot_enter_the_serialized_type() {
        for v in [
            vec![],
            vec![0.0, 0.0],
            vec![f32::NAN],
            vec![f32::INFINITY],
            vec![1.0; MAX_EMBEDDING_DIM + 1],
        ] {
            assert!(FiniteEmbedding::new(v).is_err());
        }
        assert!(serde_json::from_str::<FiniteEmbedding>("[null]").is_err());
        assert!(serde_json::from_str::<FiniteEmbedding>("[0,0]").is_err());
        assert_eq!(
            serde_json::to_string(&embedding(&[0.5, -0.5])).unwrap(),
            "[0.5,-0.5]"
        );
    }

    #[test]
    fn three_second_tail_is_retained_and_shorter_tail_is_explicit() {
        let p =
            plan_coherence(&[source(1, 0, 208_000)], 208_000, PlannerConfig::default()).unwrap();
        assert_eq!(
            p.candidates.iter().map(|w| w.source).collect::<Vec<_>>(),
            vec![
                span(0, 80_000),
                span(80_000, 160_000),
                span(160_000, 208_000)
            ]
        );
        let p =
            plan_coherence(&[source(1, 0, 207_999)], 207_999, PlannerConfig::default()).unwrap();
        assert_eq!(p.unsupported, vec![source(1, 160_000, 207_999)]);
    }

    #[test]
    fn planner_never_bridges_short_turns_or_pads_them() {
        let p = plan_coherence(
            &[source(0, 0, 32_000), source(1, 100_000, 132_000)],
            132_000,
            PlannerConfig::default(),
        )
        .unwrap();
        assert!(p.selected.is_empty());
        assert_eq!(p.unsupported.len(), 2);
        assert_eq!(p.source_samples, 64_000);
    }

    #[test]
    fn planner_covers_late_clock_regions_and_caps_each_stratum() {
        let p = plan_coherence(
            &[source(0, 0, 16_000_000)],
            16_000_000,
            PlannerConfig::default(),
        )
        .unwrap();
        assert_eq!(p.selected.len(), 16);
        assert_eq!(p.speech_occupied_strata, BTreeSet::from([0, 1, 2, 3]));
        for q in 0..4 {
            let candidates: Vec<_> = p
                .candidates
                .iter()
                .filter(|w| w.source_clock_stratum == q)
                .collect();
            let selected: Vec<_> = p
                .selected
                .iter()
                .filter(|w| w.source_clock_stratum == q)
                .collect();
            assert_eq!(selected.first(), candidates.first());
            assert_eq!(selected.last(), candidates.last());
        }
        assert!(p.selected.last().unwrap().source.start_sample > 15_000_000);
    }

    #[test]
    fn reordered_authoritative_inputs_keep_identical_plans() {
        let a = [source(8, 200_000, 280_000), source(2, 0, 80_000)];
        assert_eq!(
            plan_coherence(&a, 280_000, PlannerConfig::default()).unwrap(),
            plan_coherence(&[a[1], a[0]], 280_000, PlannerConfig::default()).unwrap()
        );
    }

    #[test]
    fn empty_overlapping_duplicate_and_out_of_bounds_sources_are_invalid() {
        for a in [
            vec![],
            vec![source(0, 10, 10)],
            vec![source(0, 0, 60_000), source(1, 50_000, 80_000)],
            vec![source(0, 0, 40_000), source(0, 50_000, 80_000)],
            vec![source(0, 0, 100_001)],
        ] {
            assert!(plan_coherence(&a, 100_000, PlannerConfig::default()).is_err());
        }
    }

    #[test]
    fn source_bounds_and_midpoint_arithmetic_do_not_overflow() {
        let start = u64::MAX - 320_000;
        let p = plan_coherence(
            &[source(1, start, u64::MAX)],
            u64::MAX,
            PlannerConfig::default(),
        )
        .unwrap();
        assert_eq!(p.source_samples, 320_000);
        assert_eq!(p.selected.len(), 4);
    }

    #[test]
    fn union_coverage_does_not_count_duplicate_overlap_or_gaps_twice() {
        assert_eq!(
            unique_sample_coverage(&[span(0, 10), span(5, 20), span(0, 10), span(30, 40)]).unwrap(),
            30
        );
        assert!(unique_sample_coverage(&[span(4, 4)]).is_err());
    }

    #[test]
    fn sufficient_without_a_rule_never_claims_consistency() {
        let p =
            plan_coherence(&[source(0, 0, 320_000)], 320_000, PlannerConfig::default()).unwrap();
        assert_eq!(
            validate_cluster(&rows(&p), &p, config()).status,
            EvidenceStatus::SufficientUnchecked
        );
    }

    #[test]
    fn a_minority_contradiction_vetoes_a_dominant_centroid() {
        let p =
            plan_coherence(&[source(0, 0, 320_000)], 320_000, PlannerConfig::default()).unwrap();
        let mut r = rows(&p);
        r.last_mut().unwrap().result = valid(&[0.0, 1.0]);
        let mut c = config();
        c.coherence_rule = Some(CoherenceRule {
            statistic: CoherenceStatistic::Minimum,
            minimum_cosine: 0.5,
        });
        let result = validate_cluster(&r, &p, c);
        assert_eq!(result.status, EvidenceStatus::Conflict);
        assert_eq!(result.pairwise.unwrap().minimum, 0.0);
    }

    #[test]
    fn explicit_invalid_windows_remain_in_the_denominator() {
        let p =
            plan_coherence(&[source(0, 0, 320_000)], 320_000, PlannerConfig::default()).unwrap();
        let mut r = rows(&p);
        r[0].result = invalid();
        let a = validate_cluster(&r, &p, config());
        assert_eq!(a.status, EvidenceStatus::Insufficient);
        assert_eq!(a.coverage.selected_windows, 4);
        assert_eq!(a.coverage.valid_windows, 3);
        assert!(serde_json::to_string(&r).unwrap().contains("nonfinite"));
    }

    #[test]
    fn altered_missing_duplicate_or_wrong_dim_evidence_is_invalid() {
        let p =
            plan_coherence(&[source(0, 0, 320_000)], 320_000, PlannerConfig::default()).unwrap();
        let mut r = rows(&p);
        r.pop();
        assert_eq!(
            validate_cluster(&r, &p, config()).status,
            EvidenceStatus::Invalid
        );
        let mut r = rows(&p);
        r[1] = r[0].clone();
        assert_eq!(
            validate_cluster(&r, &p, config()).status,
            EvidenceStatus::Invalid
        );
        let mut r = rows(&p);
        r[0].window.source.end_sample -= 1;
        assert_eq!(
            validate_cluster(&r, &p, config()).status,
            EvidenceStatus::Invalid
        );
        let mut r = rows(&p);
        r[0].result = valid(&[1.0]);
        assert_eq!(
            validate_cluster(&r, &p, config()).status,
            EvidenceStatus::Invalid
        );
    }

    #[test]
    fn late_short_speech_makes_its_uninspected_stratum_insufficient() {
        let p = plan_coherence(
            &[source(0, 0, 320_000), source(1, 1_000_000, 1_016_000)],
            1_016_000,
            PlannerConfig::default(),
        )
        .unwrap();
        let a = validate_cluster(&rows(&p), &p, config());
        assert!(a.coverage.occupied_strata.contains(&3));
        assert!(!a.coverage.valid_strata.contains(&3));
        assert_eq!(a.status, EvidenceStatus::Insufficient);
        assert_eq!(a.coverage.unsupported_source_samples, 16_000);
        let mut eligible = config();
        eligible.stratum_requirement = StratumRequirement::AllEligibleOccupied;
        assert_eq!(
            validate_cluster(&rows(&p), &p, eligible).status,
            EvidenceStatus::SufficientUnchecked
        );
    }

    #[test]
    fn recovery_keeps_valid_primary_bits_and_ignores_unneeded_retries() {
        let primary = [0.2, 0.7];
        let r =
            recover_embedding(Some(&primary), 960_000, &[], 1_000_000, recovery_policy()).unwrap();
        assert_eq!(r.query_source, QuerySource::Primary);
        assert_eq!(r.embedding.unwrap().as_slice(), primary);
    }

    #[test]
    fn two_finite_tiles_recover_an_invalid_pool_with_full_declared_coverage() {
        let r = recover_embedding(
            Some(&[f32::NAN, 0.0]),
            960_000,
            &tiles(),
            1_000_000,
            recovery_policy(),
        )
        .unwrap();
        assert_eq!(r.query_source, QuerySource::Recovered);
        assert_eq!(r.valid_samples, 960_000);
        assert_eq!(r.embedding.unwrap().as_slice(), [1.0, 0.0]);
    }

    #[test]
    fn invalid_or_missing_recovery_tiles_cannot_claim_full_coverage() {
        let mut t = tiles();
        t[1].result = invalid();
        assert_eq!(
            recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy())
                .unwrap()
                .query_source,
            QuerySource::Unavailable
        );
        t.pop();
        assert!(recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy()).is_err());
        assert_eq!(
            recover_embedding(None, 960_000, &[], 1_000_000, recovery_policy())
                .unwrap()
                .query_source,
            QuerySource::Unavailable
        );
    }

    #[test]
    fn recovery_order_is_deterministic_and_duplicate_samples_are_rejected() {
        let mut t = tiles();
        t[1].result = valid(&[0.8, 0.6]);
        let a = recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy()).unwrap();
        t.reverse();
        assert_eq!(
            a,
            recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy()).unwrap()
        );
        t[1] = t[0].clone();
        assert!(recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy()).is_err());
    }

    #[test]
    fn recovery_rejects_bad_mappings_and_uses_f64_for_extreme_components() {
        let mut t = tiles();
        t[0].source_spans[0].end_sample -= 1;
        assert!(recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy()).is_err());
        let mut t = tiles();
        for w in &mut t {
            w.result = valid(&[f32::MAX, 0.0]);
        }
        assert_eq!(
            recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy())
                .unwrap()
                .embedding
                .unwrap()
                .as_slice(),
            [1.0, 0.0]
        );
    }

    #[test]
    fn recovery_uses_declared_duration_weights_and_rejects_source_reuse() {
        let mut t = tiles();
        t[0].pool = span(0, 320_000);
        t[0].source_spans = vec![span(100, 320_100)];
        t[1].pool = span(320_000, 960_000);
        t[1].source_spans = vec![span(500_000, 1_140_000)];
        t[1].result = valid(&[0.0, 1.0]);
        let r = recover_embedding(None, 960_000, &t, 1_200_000, recovery_policy()).unwrap();
        let e = r.embedding.unwrap();
        assert!((e.as_slice()[0] - 1.0 / 5.0f32.sqrt()).abs() < 1e-6);
        assert!((e.as_slice()[1] - 2.0 / 5.0f32.sqrt()).abs() < 1e-6);
        let mut t = tiles();
        t[1].source_spans = t[0].source_spans.clone();
        assert!(recover_embedding(None, 960_000, &t, 1_000_000, recovery_policy()).is_err());
    }

    #[test]
    fn p10_is_linear_r7_and_evidence_order_does_not_change_assessment() {
        let metrics = pairwise_metrics(&[
            &embedding(&[1.0, 0.0]),
            &embedding(&[0.6, 0.8]),
            &embedding(&[0.0, 1.0]),
        ])
        .unwrap();
        assert!((metrics.p10 - 0.12).abs() < 1e-8);
        assert_eq!(metrics.pairs, 3);
        let p =
            plan_coherence(&[source(0, 0, 320_000)], 320_000, PlannerConfig::default()).unwrap();
        let mut r = rows(&p);
        let a = validate_cluster(&r, &p, config());
        r.reverse();
        assert_eq!(a, validate_cluster(&r, &p, config()));
    }

    #[test]
    fn short_recovery_cannot_use_finite_tiles_to_bypass_duration_preconditions() {
        let t = vec![
            RecoveryWindow {
                pool: span(0, 32_000),
                source_spans: vec![span(0, 32_000)],
                result: valid(&[1.0, 0.0]),
            },
            RecoveryWindow {
                pool: span(32_000, 64_000),
                source_spans: vec![span(40_000, 72_000)],
                result: valid(&[1.0, 0.0]),
            },
        ];
        assert_eq!(
            recover_embedding(None, 64_000, &t, 72_000, recovery_policy())
                .unwrap()
                .query_source,
            QuerySource::Unavailable
        );
        assert_eq!(
            recover_embedding(Some(&[0.3, 0.4]), 64_000, &[], 72_000, recovery_policy())
                .unwrap()
                .query_source,
            QuerySource::Primary
        );
    }

    #[test]
    fn envelope_version_identity_and_bounds_are_not_implicit_proof() {
        let hash = "a".repeat(64);
        let mut e = VoiceEvidence {
            version: EVIDENCE_VERSION,
            model_id: "titanet-small".into(),
            model_sha256: hash.clone(),
            policy_id: "explicit-dev-frozen-policy".into(),
            clusters: BTreeMap::new(),
        };
        assert!(
            validate_envelope(&e, "titanet-small", &hash, "explicit-dev-frozen-policy").is_ok()
        );
        e.version += 1;
        assert!(
            validate_envelope(&e, "titanet-small", &hash, "explicit-dev-frozen-policy").is_err()
        );
        e.version = EVIDENCE_VERSION;
        assert!(
            validate_envelope(&e, "different-model", &hash, "explicit-dev-frozen-policy").is_err()
        );
    }

    #[test]
    #[ignore = "set BAZZA_TEST_VOICE_PLANNER_GOLDEN to the independently generated Python golden JSON"]
    fn matches_frozen_python_planner_golden_cases() {
        let file = std::env::var("BAZZA_TEST_VOICE_PLANNER_GOLDEN").expect("golden path required");
        let golden: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        for case in golden["cases"].as_array().unwrap() {
            let intervals: Vec<_> = case["intervals"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    source(
                        r["turn_index"].as_u64().unwrap() as u32,
                        r["source_start_sample"].as_u64().unwrap(),
                        r["source_end_sample"].as_u64().unwrap(),
                    )
                })
                .collect();
            let bound = intervals.iter().map(|s| s.span.end_sample).max().unwrap();
            let actual = plan_coherence(&intervals, bound, PlannerConfig::default()).unwrap();
            let expected = &case["expected"];
            for (key, values) in [
                ("candidates", &actual.candidates),
                ("selected", &actual.selected),
            ] {
                let rows = expected[key].as_array().unwrap();
                assert_eq!(values.len(), rows.len(), "{} {key}", case["pool_id"]);
                for (a, e) in values.iter().zip(rows) {
                    assert_eq!(
                        a.id.turn_index,
                        u32::try_from(e["turn_index"].as_u64().unwrap()).unwrap()
                    );
                    assert_eq!(
                        a.id.tile_index,
                        e["window_id"]
                            .as_str()
                            .unwrap()
                            .rsplit("__tile-")
                            .next()
                            .unwrap()
                            .parse::<u32>()
                            .unwrap()
                    );
                    assert_eq!(
                        a.source,
                        span(
                            e["source_start_sample"].as_u64().unwrap(),
                            e["source_end_sample"].as_u64().unwrap()
                        )
                    );
                    assert_eq!(
                        u64::from(a.source_clock_stratum),
                        e["source_clock_stratum"].as_u64().unwrap()
                    );
                }
            }
            let strata: BTreeSet<_> = expected["speech_occupied_strata"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect();
            assert_eq!(actual.speech_occupied_strata, strata);
        }
    }
}
