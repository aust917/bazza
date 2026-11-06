//! Conservative open-set matching against independently enrolled voice samples.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownSpeaker {
    pub id: String,
    pub name: String,
    /// Stored centre, used to rank unsupported legacy/single-observation profiles.
    /// Valid samples take precedence over this potentially blended centre.
    pub embedding: Vec<f32>,
    /// Compatible independent, nonlegacy observations supplied by the caller.
    /// Provenance deduplication belongs to storage; direction duplicates are
    /// additionally rejected here so copied/scaled vectors cannot add evidence.
    #[serde(default)]
    pub samples: Vec<Vec<f32>>,
}

// Numerical equality only, not a fitted verification or runner-up margin.
const NUMERICAL_EPSILON: f32 = 1e-6;

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() || !a.iter().chain(b).all(|x| x.is_finite()) {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Mean vector, L2-normalised. Empty input gives an empty vector.
pub fn centroid(embs: &[Vec<f32>]) -> Vec<f32> {
    let Some(first) = embs.first() else {
        return Vec::new();
    };
    let dim = first.len();
    let mut acc = vec![0.0f32; dim];
    let mut n = 0usize;
    for e in embs.iter().filter(|e| e.len() == dim) {
        for (a, x) in acc.iter_mut().zip(e) {
            *a += x;
        }
        n += 1;
    }
    if n == 0 {
        return Vec::new();
    }
    let norm = acc.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    acc.iter()
        .map(|x| x / (n as f32) / (norm / n as f32))
        .collect()
}

fn normalized(embedding: &[f32], dimension: usize) -> Option<Vec<f32>> {
    if embedding.is_empty()
        || embedding.len() != dimension
        || !embedding.iter().all(|x| x.is_finite())
    {
        return None;
    }
    // f64 avoids overflowing/underflowing a valid finite f32 observation's norm.
    let norm = embedding
        .iter()
        .map(|x| (*x as f64).powi(2))
        .sum::<f64>()
        .sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    Some(
        embedding
            .iter()
            .map(|x| (*x as f64 / norm) as f32)
            .collect(),
    )
}

fn normalized_supports(samples: &[Vec<f32>], dimension: usize) -> Option<Vec<Vec<f32>>> {
    let mut samples = samples
        .iter()
        .map(|sample| normalized(sample, dimension))
        .collect::<Option<Vec<_>>>()?;
    // Stable arithmetic and duplicate handling regardless of database row order.
    samples.sort_by(|a, b| {
        a.iter()
            .zip(b)
            .map(|(x, y)| x.total_cmp(y))
            .find(|order| !order.is_eq())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut unique: Vec<Vec<f32>> = Vec::with_capacity(samples.len());
    for sample in samples {
        if !unique.iter().any(|other| {
            other
                .iter()
                .zip(&sample)
                .all(|(a, b)| (*a - *b).abs() <= f32::EPSILON)
        }) {
            unique.push(sample);
        }
    }
    Some(unique)
}

fn support_centre<'a>(samples: impl Iterator<Item = &'a Vec<f32>>) -> Option<Vec<f32>> {
    let mut mean = Vec::<f64>::new();
    for sample in samples {
        if mean.is_empty() {
            mean.resize(sample.len(), 0.0);
        }
        for (sum, value) in mean.iter_mut().zip(sample) {
            *sum += *value as f64;
        }
    }
    let norm = mean.iter().map(|x| x * x).sum::<f64>().sqrt();
    if !norm.is_finite() || norm == 0.0 {
        return None;
    }
    Some(mean.iter().map(|x| (x / norm) as f32).collect())
}

/// Enrollment-only lower bound: each sample must agree with the other samples,
/// without including itself in its reference centre. Two supports reduce to
/// their pairwise similarity. This is an empirical envelope, not a probability.
fn acceptance_floor(samples: &[Vec<f32>], threshold: f32) -> Option<f32> {
    if samples.len() < 2 {
        return None;
    }
    for (index, first) in samples.iter().enumerate() {
        for second in &samples[index + 1..] {
            let score = cosine(first, second);
            if !score.is_finite() || score <= 0.0 || score < threshold {
                return None;
            }
        }
    }
    let mut floor = f32::INFINITY;
    for (index, sample) in samples.iter().enumerate() {
        let others = support_centre(
            samples
                .iter()
                .enumerate()
                .filter_map(|(other, sample)| (other != index).then_some(sample)),
        )?;
        let score = cosine(sample, &others);
        if !score.is_finite() || score <= 0.0 {
            return None;
        }
        floor = floor.min(score);
    }
    Some(floor.max(threshold))
}

type RankedProfile<'a> = (&'a KnownSpeaker, f32, Option<Vec<Vec<f32>>>);

fn ranked_profiles<'a>(query: &[f32], known: &'a [KnownSpeaker]) -> Vec<RankedProfile<'a>> {
    let mut candidates = known.iter().filter_map(|speaker| {
        let samples = normalized_supports(&speaker.samples, query.len());
        let centre = samples.as_ref().and_then(|samples| support_centre(samples.iter()))
            .or_else(|| normalized(&speaker.embedding, query.len()))?;
        let score = cosine(query, &centre);
        score.is_finite().then_some((speaker, score, samples))
    }).collect::<Vec<_>>();
    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    candidates
}

/// Relative rank only, including unsupported nearest profiles. This cannot
/// establish identity; it can only expose contradictions in an accepted query.
pub fn nearest_profile<'a>(emb: &[f32], known: &'a [KnownSpeaker]) -> Option<(&'a KnownSpeaker, f32)> {
    let query = normalized(emb, emb.len())?;
    let candidates = ranked_profiles(&query, known);
    let best = candidates.first()?;
    if best.1 <= 0.0 || candidates.get(1).is_some_and(|next| (best.1 - next.1).abs() <= NUMERICAL_EPSILON) {
        return None;
    }
    Some((best.0, best.1))
}

pub(crate) fn rankable_profile_count(emb: &[f32], known: &[KnownSpeaker]) -> usize {
    normalized(emb, emb.len()).map(|query| ranked_profiles(&query, known).len()).unwrap_or(0)
}

/// Suggest only an independently supported, unambiguous nearest identity.
/// The nearest profile is ranked before verification. Unsupported/rejected
/// nearest profiles cannot hand a query to a weaker candidate. Enrollment-only
/// leave-one-out floors and ambiguity rules are unchanged by temporal checks.
pub fn best_match<'a>(
    emb: &[f32],
    known: &'a [KnownSpeaker],
    threshold: f32,
) -> Option<(&'a KnownSpeaker, f32)> {
    if !threshold.is_finite() {
        return None;
    }
    let query = normalized(emb, emb.len())?;
    let candidates = ranked_profiles(&query, known);
    let best = candidates.first()?;
    if best.1 <= 0.0 || best.1 < threshold {
        return None;
    }
    if candidates
        .get(1)
        .is_some_and(|next| (best.1 - next.1).abs() <= NUMERICAL_EPSILON)
    {
        return None;
    }
    let floor = acceptance_floor(best.2.as_ref()?, threshold)?;
    if best.1 < floor {
        return None;
    }
    if candidates.iter().skip(1).any(|(_, score, samples)| {
        samples
            .as_ref()
            .and_then(|samples| acceptance_floor(samples, threshold))
            .is_some_and(|floor| *score > 0.0 && *score >= floor)
    }) {
        return None;
    }
    Some((best.0, best.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direction(angle: f32) -> Vec<f32> {
        vec![angle.cos(), angle.sin()]
    }

    fn speaker(id: &str, angles: &[f32]) -> KnownSpeaker {
        let samples: Vec<_> = angles.iter().map(|angle| direction(*angle)).collect();
        let embedding = support_centre(samples.iter()).unwrap_or_default();
        KnownSpeaker {
            id: id.into(),
            name: id.into(),
            embedding,
            samples,
        }
    }

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn centroid_is_unit_length_mean() {
        let c = centroid(&[vec![2.0, 0.0], vec![0.0, 2.0]]);
        assert!((cosine(&c, &[1.0, 1.0]) - 1.0).abs() < 1e-5);
        let n: f32 = c.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-5);
        assert!(centroid(&[]).is_empty());
    }

    #[test]
    fn independent_known_query_uses_samples_instead_of_stale_centre() {
        let mut known = vec![speaker("Ann", &[-0.2, 0.2]), speaker("Bob", &[1.3, 1.7])];
        known[0].embedding = direction(1.5);
        // A fresh direction, distinct from both enrollment observations.
        let (matched, score) = best_match(&direction(0.1), &known, 0.62).unwrap();
        assert_eq!(matched.name, "Ann");
        assert!((score - 0.1f32.cos()).abs() < 1e-6);
        assert!(best_match(&direction(0.1), &known, 0.999).is_none());
    }

    #[test]
    fn nearest_unknown_outside_enrollment_consistency_stays_unknown() {
        let known = vec![speaker("Ann", &[-0.1, 0.1]), speaker("Bob", &[1.3, 1.7])];
        let query = direction(0.45);
        assert!(cosine(&query, &known[0].embedding) > 0.62);
        assert!(cosine(&query, &known[0].embedding) > cosine(&query, &known[1].embedding));
        assert!(best_match(&query, &known, 0.62).is_none());
    }

    #[test]
    fn leave_one_out_does_not_use_inclusive_centres_or_only_pairwise_floor() {
        let known = vec![speaker("Ann", &[-0.3, 0.0, 0.3])];
        let query = direction(0.5);
        // This is above the weakest pair, but below independent LOO support.
        assert!(
            cosine(&query, &known[0].embedding)
                > cosine(&known[0].samples[0], &known[0].samples[2])
        );
        assert!(best_match(&query, &known, 0.62).is_none());
        assert!(best_match(&direction(0.2), &known, 0.62).is_some());
    }

    #[test]
    fn duplicate_and_scaled_duplicate_samples_do_not_add_support() {
        let mut known = vec![speaker("Ann", &[0.0])];
        assert!(best_match(&direction(0.0), &known, 0.62).is_none());
        known[0].samples = vec![vec![3.0, 4.0], vec![3.0, 4.0], vec![6.0, 8.0]];
        known[0].embedding = vec![0.6, 0.8];
        assert!(best_match(&[0.6, 0.8], &known, 0.62).is_none());
        known[0].samples.clear();
        assert!(best_match(&[0.6, 0.8], &known, 0.62).is_none());
    }

    #[test]
    fn malformed_samples_and_queries_stay_unknown() {
        let known = vec![speaker("Ann", &[-0.2, 0.2])];
        assert!(best_match(&[f32::NAN, 0.0], &known, 0.62).is_none());
        assert!(best_match(&[f32::INFINITY, 0.0], &known, 0.62).is_none());
        assert!(best_match(&[0.0, 0.0], &known, 0.62).is_none());
        assert!(best_match(&[1.0], &known, 0.62).is_none());
        assert!(best_match(&[], &known, 0.62).is_none());
        assert!(best_match(&[1.0, 0.0], &known, f32::NAN).is_none());
        for invalid in [
            vec![f32::NAN, 0.0],
            vec![0.0, 0.0],
            vec![1.0],
            vec![f32::INFINITY, 0.0],
        ] {
            let mut broken = known.clone();
            broken[0].samples.push(invalid);
            assert!(best_match(&[1.0, 0.0], &broken, 0.62).is_none());
        }
    }

    #[test]
    fn inconsistent_profile_abstains_even_at_its_centroid() {
        let known = vec![speaker("Ann", &[-0.7, 0.7])];
        assert!(best_match(&[1.0, 0.0], &known, 0.62).is_none());
    }

    #[test]
    fn several_supported_candidates_abstain_despite_different_scores() {
        let known = vec![speaker("Ann", &[-0.4, 0.4]), speaker("Bob", &[0.2, 0.8])];
        let query = direction(0.15);
        assert!(
            (cosine(&query, &known[0].embedding) - cosine(&query, &known[1].embedding)).abs()
                > 0.01
        );
        assert!(best_match(&query, &known, 0.62).is_none());
    }

    #[test]
    fn unsupported_nearest_does_not_fall_through_to_supported_profile() {
        let known = vec![speaker("Ann", &[0.0]), speaker("Bob", &[0.1, 0.9])];
        let query = direction(0.05);
        assert!(best_match(&query, &known[1..], 0.62).is_some());
        assert!(best_match(&query, &known, 0.62).is_none());
    }

    #[test]
    fn rejected_nearest_does_not_fall_through_to_looser_profile() {
        let known = vec![speaker("Ann", &[-0.05, 0.05]), speaker("Bob", &[0.2, 1.0])];
        let query = direction(0.2);
        assert!(best_match(&query, &known[1..], 0.62).is_some());
        assert!(best_match(&query, &known, 0.62).is_none());
    }

    #[test]
    fn single_supported_profile_can_accept_a_fresh_query() {
        let known = vec![speaker("Ann", &[-0.2, 0.2])];
        assert_eq!(
            best_match(&direction(0.1), &known, 0.62).unwrap().0.id,
            "Ann"
        );
        // Large finite inputs normalize without overflowing their squared norm.
        assert!(best_match(&[f32::MAX, 0.0], &known, 0.62).is_some());
    }

    #[test]
    fn enrollment_and_candidate_order_do_not_change_decision() {
        let mut known = vec![
            speaker("Ann", &[-0.2, 0.0, 0.3]),
            speaker("Bob", &[1.3, 1.7]),
        ];
        let query = direction(0.1);
        let first =
            best_match(&query, &known, 0.62).map(|(speaker, score)| (speaker.id.clone(), score));
        assert!(first.is_some());
        known.reverse();
        for speaker in &mut known {
            speaker.samples.reverse();
        }
        let reordered =
            best_match(&query, &known, 0.62).map(|(speaker, score)| (speaker.id.clone(), score));
        assert_eq!(first, reordered);
    }

    #[test]
    fn indistinguishable_profiles_and_old_serialized_profiles_abstain() {
        let known = vec![speaker("Ann", &[-0.2, 0.2]), speaker("Bob", &[-0.2, 0.2])];
        assert!(best_match(&direction(0.1), &known, 0.62).is_none());
        let old: KnownSpeaker =
            serde_json::from_str(r#"{"id":"legacy","name":"Ann","embedding":[1.0,0.0]}"#).unwrap();
        assert!(old.samples.is_empty());
        assert!(best_match(&[1.0, 0.0], &[old], 0.62).is_none());
    }
}
