//! Pure retrieval scoring shared by storage and native consumers.
use crate::ChunkHit;

/// Cosine similarity; vectors are expected to be L2-normalised already, so
/// this is just a dot product with a guard for mismatched sizes.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Reciprocal rank fusion (k = 60) of two ranked lists keyed by (meeting, start).
pub fn fuse_hits(a: Vec<ChunkHit>, b: Vec<ChunkHit>, limit: usize) -> Vec<ChunkHit> {
    const K: f64 = 60.0;
    let mut scored: Vec<(f64, ChunkHit)> = Vec::new();
    for list in [a, b] {
        for (i, hit) in list.into_iter().enumerate() {
            let s = 1.0 / (K + i as f64 + 1.0);
            match scored
                .iter_mut()
                .find(|(_, h)| h.meeting_id == hit.meeting_id && h.start_ms == hit.start_ms)
            {
                Some(e) => e.0 += s,
                None => scored.push((s, hit)),
            }
        }
    }
    scored.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .take(limit)
        .map(|(s, mut h)| {
            h.rank = -s;
            h
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(m: &str, start: i64) -> ChunkHit {
        ChunkHit {
            meeting_id: m.into(),
            meeting_title: "t".into(),
            started_at: chrono::Utc::now(),
            start_ms: start,
            end_ms: start + 1,
            text: String::new(),
            rank: 0.0,
        }
    }

    #[test]
    fn rrf_interleaves_and_boosts_shared_hits() {
        let a = vec![hit("m", 0), hit("m", 100), hit("m", 200)];
        let b = vec![hit("m", 300), hit("m", 100)];
        let f = fuse_hits(a, b, 10);
        assert_eq!(f[0].start_ms, 100); // in both lists
        assert_eq!(f.len(), 4);
        assert_eq!(fuse_hits(vec![hit("m", 0)], vec![], 1).len(), 1);
    }
}
