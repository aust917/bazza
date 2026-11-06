//! Managed local voice samples. Audio stays outside meeting exports.

use crate::speakers::invalidate_pending_suggestions;
use crate::{Result, Speaker, Store, StoreError};
pub use bazza_domain::{VoiceProfile, VoiceSample};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;

/// Identity of the sole voice embedding artifact in the current model catalog.
/// A backend/artifact change must use another identity, even at the same dimension.
pub const CURRENT_VOICE_MODEL: &str =
    "titanet-small:sha256:ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e";

/// Backend-only matching evidence. Independent samples support verification;
/// unsupported profiles stay in the candidate set so matching can abstain when
/// their stored voiceprint is the nearest neighbour. Never sent over IPC.
#[derive(Clone, Debug, PartialEq)]
pub struct VoiceMatchProfile {
    pub id: String,
    pub name: String,
    pub embedding: Vec<f32>,
    pub samples: Vec<Vec<f32>>,
}

pub struct NewVoiceSample<'a> {
    pub speaker_id: &'a str,
    pub embedding: &'a [f32],
    pub model_id: &'a str,
    pub source: &'a str,
    pub source_label: &'a str,
    pub meeting_id: Option<&'a str>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub has_audio: bool,
}

fn blob(v: &[f32]) -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() }
fn unblob(v: &[u8]) -> Vec<f32> { v.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect() }
fn valid_embedding(v: &[f32]) -> bool {
    !v.is_empty() && v.len() <= 4096 && v.iter().all(|x| x.is_finite()) && v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() > 1e-12
}

fn normalized_embedding(v: &[f32]) -> Option<Vec<f32>> {
    if !valid_embedding(v) { return None; }
    let norm = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    Some(v.iter().map(|x| (*x as f64 / norm) as f32).collect())
}

fn checked_embedding(bytes: &[u8], dimension: i64) -> Option<Vec<f32>> {
    let dimension = usize::try_from(dimension).ok()?;
    if dimension == 0 || dimension > 4096 || bytes.len() != dimension * 4 { return None; }
    normalized_embedding(&unblob(bytes))
}

struct MatchSample {
    embedding: Vec<f32>,
    origin: String,
    interval: Option<(i64, i64)>,
}

/// Select non-overlapping observations without letting duplicate imports enlarge
/// the apparent support. A missing interval cannot establish a second sample
/// from the same source. File basenames are deliberately conservative because
/// the original absolute path is not retained in the database.
fn independent_samples(mut candidates: Vec<MatchSample>) -> Vec<Vec<f32>> {
    // Prefer bounded file intervals over an unbounded observation of that file.
    // Stable sorting preserves the database's order among equivalent evidence.
    candidates.sort_by_key(|sample| sample.interval.is_none());
    let mut origins: BTreeMap<String, Vec<Option<(i64, i64)>>> = BTreeMap::new();
    let mut samples: Vec<Vec<f32>> = Vec::new();
    for sample in candidates {
        if samples.iter().any(|known| known.len() == sample.embedding.len()
            && known.iter().zip(&sample.embedding).all(|(a, b)| (*a - *b).abs() <= 1e-6)) {
            continue;
        }
        let existing = origins.entry(sample.origin).or_default();
        if existing.iter().any(|previous| match (previous, sample.interval) {
            (Some((a, b)), Some((c, d))) => c < *b && d > *a,
            _ => true,
        }) {
            continue;
        }
        existing.push(sample.interval);
        samples.push(sample.embedding);
    }
    samples
}

fn normalized_centre(samples: &[Vec<f32>]) -> Option<Vec<f32>> {
    let dimension = samples.first()?.len();
    let mut mean = vec![0.0f64; dimension];
    for sample in samples {
        if sample.len() != dimension { return None; }
        for (total, value) in mean.iter_mut().zip(sample) { *total += *value as f64; }
    }
    let norm = mean.iter().map(|x| x * x).sum::<f64>().sqrt();
    if !norm.is_finite() || norm <= 1e-12 { return None; }
    Some(mean.into_iter().map(|value| (value / norm) as f32).collect())
}

fn profile(conn: &Connection, id: &str) -> Result<VoiceProfile> {
    conn.query_row(
        "SELECT id, name, embedding_model, (SELECT COUNT(*) FROM voice_samples WHERE speaker_id = speakers.id), dim > 0 FROM speakers WHERE id = ?1",
        params![id], |r| Ok(VoiceProfile { id: r.get(0)?, name: r.get(1)?, model_id: r.get(2)?, sample_count: r.get(3)?, has_voiceprint: r.get(4)? }),
    ).optional()?.ok_or_else(|| StoreError::Other("voice profile not found".into()))
}

/// Recompute from compatible samples only, with equal weight per sample.
fn recompute(conn: &Connection, id: &str) -> Result<()> {
    let p = profile(conn, id)?;
    let mut st = conn.prepare("SELECT embedding FROM voice_samples WHERE speaker_id = ?1 AND model_id = ?2 ORDER BY created_at, id")?;
    let vectors = st.query_map(params![id, p.model_id], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?.iter().map(|v| unblob(v)).collect::<Vec<_>>();
    let mut mean = Vec::<f64>::new();
    for v in vectors.iter().filter(|v| valid_embedding(v)) {
        if mean.is_empty() { mean.resize(v.len(), 0.0); }
        if v.len() != mean.len() { return Err(StoreError::Other("incompatible voice sample dimensions".into())); }
        let norm = v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
        for (a, x) in mean.iter_mut().zip(v) { *a += *x as f64 / norm; }
    }
    let norm = mean.iter().map(|x| x * x).sum::<f64>().sqrt();
    let emb: Vec<f32> = if norm > 1e-12 { mean.iter().map(|x| (x / norm) as f32).collect() } else { Vec::new() };
    conn.execute("UPDATE speakers SET embedding = ?2, dim = ?3 WHERE id = ?1", params![id, blob(&emb), emb.len() as i64])?;
    invalidate_pending_suggestions(conn)?;
    Ok(())
}

impl Store {
    pub fn list_voice_profiles(&self) -> Result<Vec<VoiceProfile>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare("SELECT id FROM speakers ORDER BY name COLLATE NOCASE")?;
        let ids = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter().map(|id| profile(&conn, id)).collect()
    }

    pub fn create_voice_profile(&self, name: &str) -> Result<VoiceProfile> {
        let name = validate_name(name)?;
        let conn = self.conn.lock();
        let id = uuid::Uuid::new_v4().to_string();
        conn.execute("INSERT INTO speakers(id, name, embedding, dim, created_at, embedding_model) VALUES (?1, ?2, X'', 0, ?3, ?4)", params![id, name, chrono::Utc::now(), CURRENT_VOICE_MODEL])?;
        profile(&conn, &id)
    }

    /// Rename memory for future suggestions; existing transcript labels remain user edits.
    pub fn rename_voice_profile(&self, id: &str, name: &str) -> Result<VoiceProfile> {
        let name = validate_name(name)?;
        let conn = self.conn.lock();
        if conn.execute("UPDATE speakers SET name = ?2 WHERE id = ?1", params![id, name])? == 0 {
            return Err(StoreError::Other("voice profile not found".into()));
        }
        profile(&conn, id)
    }

    pub fn list_compatible_speakers(&self, model_id: &str) -> Result<Vec<Speaker>> {
        let conn = self.conn.lock();
        let mut st = conn.prepare("SELECT id, name, embedding FROM speakers WHERE embedding_model = ?1 AND dim > 0 ORDER BY name")?;
        let rows = st.query_map(params![model_id], |r| Ok(Speaker { id: r.get(0)?, name: r.get(1)?, embedding: unblob(&r.get::<_, Vec<u8>>(2)?) }))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?.into_iter().filter(|s| valid_embedding(&s.embedding)).collect())
    }

    /// Independent evidence for open-set speaker verification, kept separate from
    /// the user's complete sample collection and its display/storage centroid.
    /// Observations from the recording being matched cannot verify that recording.
    pub fn list_voice_match_profiles(&self, model_id: &str, exclude_meeting_id: Option<&str>) -> Result<Vec<VoiceMatchProfile>> {
        let conn = self.conn.lock();
        let mut profiles_statement = conn.prepare("SELECT id, name, embedding, dim FROM speakers WHERE embedding_model = ?1 ORDER BY name COLLATE NOCASE, id")?;
        let profiles = profiles_statement.query_map(params![model_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, i64>(3)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut result = Vec::with_capacity(profiles.len());
        let mut samples_statement = conn.prepare("SELECT embedding, dim, source, source_label, meeting_id, start_ms, end_ms FROM voice_samples WHERE speaker_id = ?1 AND model_id = ?2 AND source <> 'legacy' ORDER BY created_at, id")?;
        for (id, name, stored, dimension) in profiles {
            let rows = samples_statement.query_map(params![id, model_id], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, Option<String>>(4)?, row.get::<_, Option<i64>>(5)?, row.get::<_, Option<i64>>(6)?))
            })?.collect::<rusqlite::Result<Vec<_>>>()?;
            let fallback = checked_embedding(&stored, dimension).unwrap_or_default();
            let mut expected_dimension = (!fallback.is_empty()).then_some(fallback.len());
            let mut candidates = Vec::new();
            for (bytes, sample_dimension, source, label, meeting_id, start, end) in rows {
                let meeting_origin = meeting_id.as_deref().map(str::trim);
                // Without an origin we cannot establish independence or rule out
                // an observation of the recording currently being matched.
                if source == "meeting" && meeting_origin.is_none_or(str::is_empty) { continue; }
                if exclude_meeting_id.is_some_and(|excluded| meeting_origin == Some(excluded)) { continue; }
                let Some(embedding) = checked_embedding(&bytes, sample_dimension) else { continue };
                if expected_dimension.is_some_and(|expected| expected != embedding.len()) { continue; }
                expected_dimension = Some(embedding.len());
                let (origin, interval) = match source.as_str() {
                    // A single meeting remains one observation, even if its
                    // speaker was renamed or sampled several times.
                    "meeting" => (format!("meeting:{}", meeting_origin.unwrap_or_default()), None),
                    "file" => {
                        let basename = label.rsplit(['/', '\\']).next().unwrap_or("").trim().to_lowercase();
                        let interval = match (start, end) {
                            (Some(a), Some(b)) if a >= 0 && b.checked_sub(a).is_some_and(|duration| (1000..=120_000).contains(&duration)) => Some((a, b)),
                            _ => None,
                        };
                        (format!("file:{basename}"), interval)
                    }
                    _ => continue,
                };
                candidates.push(MatchSample { embedding, origin, interval });
            }
            let mut samples = independent_samples(candidates);
            let centre = normalized_centre(&samples);
            let embedding = match centre {
                Some(centre) if samples.len() >= 2 => centre,
                centre => {
                    if samples.len() >= 2 { samples.clear(); }
                    if !fallback.is_empty() { fallback } else { centre.unwrap_or_default() }
                }
            };
            result.push(VoiceMatchProfile { id, name, embedding, samples });
        }
        Ok(result)
    }

    pub fn list_voice_samples(&self, speaker_id: &str) -> Result<Vec<VoiceSample>> {
        let conn = self.conn.lock();
        profile(&conn, speaker_id)?;
        let mut st = conn.prepare("SELECT id, speaker_id, model_id, source, source_label, meeting_id, start_ms, end_ms, has_audio, created_at FROM voice_samples WHERE speaker_id = ?1 ORDER BY created_at, id")?;
        let samples = st.query_map(params![speaker_id], |r| Ok(VoiceSample { id: r.get(0)?, speaker_id: r.get(1)?, model_id: r.get(2)?, source: r.get(3)?, source_label: r.get(4)?, meeting_id: r.get(5)?, start_ms: r.get(6)?, end_ms: r.get(7)?, has_audio: r.get(8)?, created_at: r.get(9)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(samples)
    }

    pub fn get_voice_sample(&self, id: &str) -> Result<Option<VoiceSample>> {
        Ok(self.conn.lock().query_row("SELECT id, speaker_id, model_id, source, source_label, meeting_id, start_ms, end_ms, has_audio, created_at FROM voice_samples WHERE id = ?1", params![id], |r| Ok(VoiceSample { id: r.get(0)?, speaker_id: r.get(1)?, model_id: r.get(2)?, source: r.get(3)?, source_label: r.get(4)?, meeting_id: r.get(5)?, start_ms: r.get(6)?, end_ms: r.get(7)?, has_audio: r.get(8)?, created_at: r.get(9)? })) .optional()?)
    }

    /// `id` also names the audio file; never accept path components here.
    pub fn add_voice_sample(&self, id: &str, sample: &NewVoiceSample<'_>) -> Result<VoiceSample> {
        uuid::Uuid::parse_str(id).map_err(|_| StoreError::Other("invalid voice sample id".into()))?;
        if !valid_embedding(sample.embedding) { return Err(StoreError::Other("voiceprint must be finite and nonzero".into())); }
        if !matches!(sample.source, "meeting" | "file") { return Err(StoreError::Other("invalid voice sample source".into())); }
        match (sample.start_ms, sample.end_ms) {
            (Some(a), Some(b)) if a >= 0 && b.checked_sub(a).is_some_and(|d| (1000..=120_000).contains(&d)) => {},
            _ => return Err(StoreError::Other("voice sample needs a valid interval of at most 120 seconds".into())),
        }
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let p = profile(&tx, sample.speaker_id)?;
        if p.model_id != sample.model_id { return Err(StoreError::Other("voice sample uses a different embedding model".into())); }
        let dim: Option<i64> = tx.query_row("SELECT dim FROM voice_samples WHERE speaker_id = ?1 AND model_id = ?2 LIMIT 1", params![sample.speaker_id, sample.model_id], |r| r.get(0)).optional()?;
        if dim.is_some_and(|d| d != sample.embedding.len() as i64) { return Err(StoreError::Other("voice sample has incompatible dimensions".into())); }
        tx.execute("INSERT INTO voice_samples(id, speaker_id, embedding, dim, model_id, source, source_label, meeting_id, start_ms, end_ms, has_audio, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)", params![id, sample.speaker_id, blob(sample.embedding), sample.embedding.len() as i64, sample.model_id, sample.source, sample.source_label, sample.meeting_id, sample.start_ms, sample.end_ms, sample.has_audio, chrono::Utc::now().to_rfc3339()])?;
        recompute(&tx, sample.speaker_id)?;
        tx.commit()?;
        drop(conn);
        self.list_voice_samples(sample.speaker_id)?.into_iter().find(|s| s.id == id).ok_or_else(|| StoreError::Other("voice sample not found after insert".into()))
    }

    pub fn delete_voice_sample(&self, id: &str) -> Result<VoiceSample> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let speaker_id: String = tx.query_row("SELECT speaker_id FROM voice_samples WHERE id = ?1", params![id], |r| r.get(0)).optional()?.ok_or_else(|| StoreError::Other("voice sample not found".into()))?;
        let result = tx.query_row("SELECT id, speaker_id, model_id, source, source_label, meeting_id, start_ms, end_ms, has_audio, created_at FROM voice_samples WHERE id = ?1", params![id], |r| Ok(VoiceSample { id: r.get(0)?, speaker_id: r.get(1)?, model_id: r.get(2)?, source: r.get(3)?, source_label: r.get(4)?, meeting_id: r.get(5)?, start_ms: r.get(6)?, end_ms: r.get(7)?, has_audio: r.get(8)?, created_at: r.get(9)? }))?;
        tx.execute("DELETE FROM voice_samples WHERE id = ?1", params![id])?;
        recompute(&tx, &speaker_id)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn recompute_voice_profile(&self, id: &str) -> Result<VoiceProfile> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        recompute(&tx, id)?;
        let p = profile(&tx, id)?;
        tx.commit()?;
        Ok(p)
    }
}

fn validate_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 200 || crate::speakers::is_placeholder(name) {
        return Err(StoreError::Other("enter a person's name (at most 200 characters)".into()));
    }
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn add(st: &Store, id: &str, emb: &[f32]) -> VoiceSample {
        st.add_voice_sample(&uuid::Uuid::new_v4().to_string(), &NewVoiceSample { speaker_id: id, embedding: emb, model_id: CURRENT_VOICE_MODEL, source: "file", source_label: "test.wav", meeting_id: None, start_ms: Some(0), end_ms: Some(2000), has_audio: true }).unwrap()
    }

    struct SampleOrigin<'a> {
        source: &'a str,
        label: &'a str,
        meeting: Option<&'a str>,
        interval: Option<(i64, i64)>,
    }

    fn seed_match_sample(st: &Store, speaker: &str, id: &str, embedding: &[f32], origin: SampleOrigin<'_>) {
        let (start, end) = origin.interval.map_or((None, None), |(a, b)| (Some(a), Some(b)));
        st.conn.lock().execute(
            "INSERT INTO voice_samples(id, speaker_id, embedding, dim, model_id, source, source_label, meeting_id, start_ms, end_ms, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '2026-01-01')",
            params![id, speaker, blob(embedding), embedding.len() as i64, CURRENT_VOICE_MODEL, origin.source, origin.label, origin.meeting, start, end],
        ).unwrap();
    }

    fn file(label: &str, interval: Option<(i64, i64)>) -> SampleOrigin<'_> {
        SampleOrigin { source: "file", label, meeting: None, interval }
    }

    fn meeting(id: Option<&str>) -> SampleOrigin<'_> {
        SampleOrigin { source: "meeting", label: "Speaker 1", meeting: id, interval: None }
    }

    #[test]
    fn match_profiles_use_independent_normalized_samples_and_keep_unsupported_candidates() {
        let st = Store::open_in_memory().unwrap();
        let ann = st.create_voice_profile("Ann").unwrap();
        seed_match_sample(&st, &ann.id, "a", &[2.0, 0.0], file("a.wav", Some((0, 2000))));
        seed_match_sample(&st, &ann.id, "b", &[3.0, 3.0], file("b.wav", Some((0, 2000))));
        seed_match_sample(&st, &ann.id, "copied", &[20.0, 0.0], file("renamed-copy.wav", Some((0, 2000))));
        seed_match_sample(&st, &ann.id, "legacy", &[0.0, 4.0], SampleOrigin { source: "legacy", label: "old", meeting: None, interval: None });
        // The stored centroid may contain legacy or repeated observations. It is
        // preserved for the user's data, but is not the verified match centre.
        st.conn.lock().execute("UPDATE speakers SET embedding = ?2, dim = 2 WHERE id = ?1", params![ann.id, blob(&[0.0, 1.0])]).unwrap();
        let bob = st.create_voice_profile("Bob").unwrap();
        st.conn.lock().execute("UPDATE speakers SET embedding = ?2, dim = 2 WHERE id = ?1", params![bob.id, blob(&[0.0, 2.0])]).unwrap();
        seed_match_sample(&st, &bob.id, "bob-legacy", &[0.0, 2.0], SampleOrigin { source: "legacy", label: "old", meeting: None, interval: None });
        let empty = st.create_voice_profile("Empty").unwrap();
        let profiles = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap();
        assert_eq!(profiles.len(), 3);
        let ann_match = profiles.iter().find(|p| p.id == ann.id).unwrap();
        assert_eq!(ann_match.samples.len(), 2);
        assert!((ann_match.embedding[0] - 0.9238795).abs() < 1e-6);
        assert!((ann_match.embedding[1] - 0.38268343).abs() < 1e-6);
        assert_eq!(profiles.iter().find(|p| p.id == bob.id).unwrap().embedding, vec![0.0, 1.0]);
        assert!(profiles.iter().find(|p| p.id == bob.id).unwrap().samples.is_empty());
        assert!(profiles.iter().find(|p| p.id == empty.id).unwrap().embedding.is_empty());
        assert_eq!(st.list_speakers().unwrap().into_iter().find(|p| p.id == ann.id).unwrap().embedding, vec![0.0, 1.0]);
        assert_eq!(st.list_voice_samples(&ann.id).unwrap().len(), 4);
    }

    #[test]
    fn match_support_deduplicates_file_origins_and_overlaps_but_keeps_disjoint_intervals() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        seed_match_sample(&st, &p.id, "a", &[1.0, 0.0], file("C:\\first\\SAME.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "b-overlap", &[0.99, 0.1], file("/another/same.wav", Some((1000, 3000))));
        seed_match_sample(&st, &p.id, "c-disjoint", &[0.9, 0.2], file("same.wav", Some((2000, 4000))));
        seed_match_sample(&st, &p.id, "d-unknown", &[0.8, 0.3], file("same.wav", None));
        seed_match_sample(&st, &p.id, "e-other", &[0.7, 0.4], file("other.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "f-copy", &[10.0, 0.0], file("copy.wav", Some((0, 2000))));
        let matched = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(matched.samples.len(), 3);
        assert_eq!(st.list_voice_samples(&p.id).unwrap().len(), 6);
    }

    #[test]
    fn unknown_file_intervals_do_not_create_independent_support() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        seed_match_sample(&st, &p.id, "a-unbounded", &[1.0, 0.0], file("same.wav", None));
        seed_match_sample(&st, &p.id, "b-unbounded", &[0.99, 0.1], file("same.wav", None));
        let first = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(first.samples.len(), 1);
        seed_match_sample(&st, &p.id, "c-bounded", &[0.9, 0.2], file("same.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "d-bounded", &[0.8, 0.3], file("same.wav", Some((2000, 4000))));
        let bounded = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(bounded.samples.len(), 2);
    }

    #[test]
    fn match_support_counts_meeting_origins_once_and_excludes_the_current_meeting() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        seed_match_sample(&st, &p.id, "a", &[1.0, 0.0], meeting(Some("previous-one")));
        seed_match_sample(&st, &p.id, "b-repeat", &[0.99, 0.1], meeting(Some("previous-one")));
        seed_match_sample(&st, &p.id, "c", &[0.9, 0.2], meeting(Some("previous-two")));
        seed_match_sample(&st, &p.id, "d-current", &[0.8, 0.3], meeting(Some("current")));
        seed_match_sample(&st, &p.id, "e-no-id", &[0.7, 0.4], meeting(None));
        seed_match_sample(&st, &p.id, "f-no-id", &[0.6, 0.5], meeting(None));
        let all = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        let excluded = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, Some("current")).unwrap().remove(0);
        assert_eq!(all.samples.len(), 3);
        assert_eq!(excluded.samples.len(), 2);
        assert!(!excluded.samples.iter().any(|v| (v[0] - 0.8f32 / 0.73f32.sqrt()).abs() < 1e-6));
        assert_eq!(st.list_voice_samples(&p.id).unwrap().len(), 6);
    }

    #[test]
    fn unverifiable_meeting_origins_do_not_supply_a_second_independent_sample() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        seed_match_sample(&st, &p.id, "a-previous", &[1.0, 0.0], meeting(Some("previous")));
        seed_match_sample(&st, &p.id, "b-orphan", &[0.99, 0.1], meeting(None));
        seed_match_sample(&st, &p.id, "c-empty", &[0.9, 0.2], meeting(Some("")));
        seed_match_sample(&st, &p.id, "d-blank", &[0.8, 0.3], meeting(Some("   ")));
        let candidate = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(candidate.id, p.id);
        assert_eq!(candidate.samples, vec![vec![1.0, 0.0]]);
        let excluded = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, Some("previous")).unwrap().remove(0);
        assert!(excluded.samples.is_empty());
        assert_eq!(st.list_voice_samples(&p.id).unwrap().len(), 4);
    }

    #[test]
    fn match_support_rejects_invalid_blobs_dimensions_and_models_without_hiding_profiles() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        add(&st, &p.id, &[1.0, 0.0]);
        seed_match_sample(&st, &p.id, "nan", &[f32::NAN, 1.0], file("nan.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "zero", &[0.0, 0.0], file("zero.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "wrong-dimension", &[1.0], file("short.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "other-model", &[0.0, 1.0], file("other.wav", Some((0, 2000))));
        st.conn.lock().execute("UPDATE voice_samples SET model_id = 'other' WHERE id = 'other-model'", []).unwrap();
        seed_match_sample(&st, &p.id, "bad-blob", &[0.7, 0.7], file("bad.wav", Some((0, 2000))));
        st.conn.lock().execute("UPDATE voice_samples SET embedding = X'0000803F01' WHERE id = 'bad-blob'", []).unwrap();
        let candidate = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(candidate.samples.len(), 1);
        assert_eq!(candidate.embedding, vec![1.0, 0.0]);
        assert!(st.list_voice_match_profiles("other", None).unwrap().is_empty());
        assert_eq!(st.list_voice_samples(&p.id).unwrap().len(), 6);
    }

    #[test]
    fn match_support_becomes_unsupported_after_deletion_without_removing_the_profile() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        let a = add(&st, &p.id, &[1.0, 0.0]);
        let b = st.add_voice_sample(&uuid::Uuid::new_v4().to_string(), &NewVoiceSample { speaker_id: &p.id, embedding: &[0.9, 0.1], model_id: CURRENT_VOICE_MODEL, source: "file", source_label: "other.wav", meeting_id: None, start_ms: Some(0), end_ms: Some(2000), has_audio: true }).unwrap();
        assert_eq!(st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap()[0].samples.len(), 2);
        st.delete_voice_sample(&b.id).unwrap();
        let candidate = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(candidate.samples.len(), 1);
        assert_eq!(candidate.embedding, vec![1.0, 0.0]);
        st.delete_voice_sample(&a.id).unwrap();
        let candidate = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert_eq!(candidate.id, p.id);
        assert!(candidate.samples.is_empty());
        assert!(candidate.embedding.is_empty());
    }

    #[test]
    fn incompatible_sample_directions_cannot_supply_a_verified_centre() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        seed_match_sample(&st, &p.id, "a", &[1.0, 0.0], file("a.wav", Some((0, 2000))));
        seed_match_sample(&st, &p.id, "b", &[-1.0, 0.0], file("b.wav", Some((0, 2000))));
        st.conn.lock().execute("UPDATE speakers SET embedding = ?2, dim = 2 WHERE id = ?1", params![p.id, blob(&[0.0, 1.0])]).unwrap();
        let candidate = st.list_voice_match_profiles(CURRENT_VOICE_MODEL, None).unwrap().remove(0);
        assert!(candidate.samples.is_empty());
        assert_eq!(candidate.embedding, vec![0.0, 1.0]);
        assert_eq!(st.list_voice_samples(&p.id).unwrap().len(), 2);
    }

    fn pending_suggestion_fixture() -> (Store, String, String, String) {
        let st = Store::open_in_memory().unwrap();
        let a = st.create_voice_profile("Ann").unwrap();
        let b = st.create_voice_profile("Bob").unwrap();
        let confirmed = st.create_voice_profile("Confirmed").unwrap();
        add(&st, &a.id, &[1.0, 0.0]);
        add(&st, &b.id, &[0.0, 1.0]);
        let meeting = crate::Meeting::new_recording("suggestion invalidation", None);
        st.create_meeting(&meeting).unwrap();
        let segments = ["Speaker 1", "Speaker 2", "Confirmed"].into_iter().enumerate().map(|(index, label)| crate::NewSegment {
            start_ms: index as i64 * 2000, end_ms: (index as i64 + 1) * 2000,
            speaker: Some(label.into()), text: format!("original words {index}"),
        }).collect::<Vec<_>>();
        st.replace_segments(&meeting.id, &segments).unwrap();
        st.set_meeting_speakers(&meeting.id, &[
            crate::MeetingSpeaker { meeting_id: meeting.id.clone(), label: "Speaker 1".into(), speaker_id: None, suggested_id: Some(a.id.clone()), suggested_name: None, suggested_score: Some(0.95) },
            crate::MeetingSpeaker { meeting_id: meeting.id.clone(), label: "Speaker 2".into(), speaker_id: None, suggested_id: Some(b.id.clone()), suggested_name: None, suggested_score: Some(0.96) },
            crate::MeetingSpeaker { meeting_id: meeting.id.clone(), label: "Confirmed".into(), speaker_id: Some(confirmed.id), suggested_id: Some(a.id.clone()), suggested_name: None, suggested_score: Some(0.97) },
        ]).unwrap();
        (st, meeting.id, a.id, b.id)
    }

    #[test]
    fn evidence_mutations_invalidate_all_pending_profiles_but_preserve_confirmations() {
        for operation in ["add", "delete-sample", "recompute", "upsert", "delete-profile"] {
            let (st, meeting, _a, b) = pending_suggestion_fixture();
            let before = st.meeting_speakers(&meeting).unwrap();
            let confirmed = before.iter().find(|row| row.label == "Confirmed").unwrap();
            let segments = st.segments(&meeting).unwrap();
            match operation {
                "add" => { add(&st, &b, &[0.1, 0.9]); }
                "delete-sample" => { st.delete_voice_sample(&st.list_voice_samples(&b).unwrap()[0].id).unwrap(); }
                "recompute" => { st.recompute_voice_profile(&b).unwrap(); }
                "upsert" => { st.upsert_speaker(&Speaker { id: b.clone(), name: "Bob".into(), embedding: vec![0.1, 0.9] }).unwrap(); }
                "delete-profile" => { st.delete_speaker(&b).unwrap(); }
                _ => unreachable!(),
            }
            let after = st.meeting_speakers(&meeting).unwrap();
            assert!(after.iter().filter(|row| row.speaker_id.is_none()).all(|row| row.suggested_id.is_none() && row.suggested_score.is_none()), "{operation}");
            assert_eq!(after.iter().find(|row| row.label == "Confirmed").unwrap(), confirmed, "{operation}");
            assert_eq!(st.segments(&meeting).unwrap(), segments, "{operation}");
        }
    }

    #[test]
    fn failed_sample_add_and_non_evidence_edits_keep_pending_suggestions() {
        let (st, meeting, _a, b) = pending_suggestion_fixture();
        let before = st.meeting_speakers(&meeting).unwrap();
        let sample_id = st.list_voice_samples(&b).unwrap()[0].id.clone();
        let mut sample = NewVoiceSample { speaker_id: &b, embedding: &[0.1, 0.9], model_id: "wrong-model", source: "file", source_label: "other.wav", meeting_id: None, start_ms: Some(0), end_ms: Some(2000), has_audio: true };
        assert!(st.add_voice_sample(&uuid::Uuid::new_v4().to_string(), &sample).is_err());
        assert_eq!(st.meeting_speakers(&meeting).unwrap(), before);
        sample.model_id = CURRENT_VOICE_MODEL;
        // This gets as far as the transaction but violates the existing sample PK.
        assert!(st.add_voice_sample(&sample_id, &sample).is_err());
        assert_eq!(st.meeting_speakers(&meeting).unwrap(), before);
        st.create_voice_profile("Empty").unwrap();
        assert_eq!(st.meeting_speakers(&meeting).unwrap(), before);
        st.rename_voice_profile(&b, "Robert").unwrap();
        let after = st.meeting_speakers(&meeting).unwrap();
        assert_eq!(after.iter().map(|row| (&row.label, &row.speaker_id, &row.suggested_id, row.suggested_score)).collect::<Vec<_>>(), before.iter().map(|row| (&row.label, &row.speaker_id, &row.suggested_id, row.suggested_score)).collect::<Vec<_>>());
    }

    #[test]
    fn confirmed_meeting_observation_invalidates_unrelated_pending_guesses() {
        let (st, meeting, a, _b) = pending_suggestion_fixture();
        let before = st.meeting_speakers(&meeting).unwrap();
        let confirmed = before.iter().find(|row| row.label == "Confirmed").unwrap();
        let segments = st.segments(&meeting).unwrap();
        let renamed = st.rename_speaker(&meeting, "Speaker 1", "Ann", Some(&[0.9, 0.1])).unwrap();
        assert_eq!(renamed.speaker.unwrap().id, a);
        let after = st.meeting_speakers(&meeting).unwrap();
        let selected = after.iter().find(|row| row.label == "Ann").unwrap();
        assert_eq!(selected.speaker_id.as_deref(), Some(a.as_str()));
        assert!(selected.suggested_id.is_none());
        assert!(selected.suggested_score.is_none());
        let unrelated = after.iter().find(|row| row.label == "Speaker 2").unwrap();
        assert!(unrelated.suggested_id.is_none());
        assert!(unrelated.suggested_score.is_none());
        assert_eq!(after.iter().find(|row| row.label == "Confirmed").unwrap(), confirmed);
        let updated = st.segments(&meeting).unwrap();
        for (old, new) in segments.iter().zip(&updated) {
            assert_eq!((old.id, old.start_ms, old.end_ms, &old.text), (new.id, new.start_ms, new.end_ms, &new.text));
            assert_eq!(new.speaker.as_deref(), if old.speaker.as_deref() == Some("Speaker 1") { Some("Ann") } else { old.speaker.as_deref() });
        }
        assert!(st.list_voice_samples(&a).unwrap().iter().any(|sample| sample.source == "meeting" && sample.meeting_id.as_deref() == Some(meeting.as_str())));
    }

    #[test]
    fn samples_recompute_and_last_deletion_forgets_voice() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        let a = add(&st, &p.id, &[2.0, 0.0]);
        let b = add(&st, &p.id, &[0.0, 3.0]);
        let known = st.list_compatible_speakers(CURRENT_VOICE_MODEL).unwrap();
        assert!((known[0].embedding[0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        st.delete_voice_sample(&b.id).unwrap();
        assert_eq!(st.list_speakers().unwrap()[0].embedding, vec![1.0, 0.0]);
        st.delete_voice_sample(&a.id).unwrap();
        assert!(!st.list_voice_profiles().unwrap()[0].has_voiceprint);
        assert!(st.list_compatible_speakers(CURRENT_VOICE_MODEL).unwrap().is_empty());
    }

    #[test]
    fn incompatible_or_invalid_samples_leave_profile_unchanged() {
        let st = Store::open_in_memory().unwrap();
        let p = st.create_voice_profile("Ann").unwrap();
        add(&st, &p.id, &[1.0, 0.0]);
        let mut sample = NewVoiceSample { speaker_id: &p.id, embedding: &[0.0, 1.0], model_id: "other-model", source: "file", source_label: "test.wav", meeting_id: None, start_ms: Some(0), end_ms: Some(2000), has_audio: true };
        assert!(st.add_voice_sample(&uuid::Uuid::new_v4().to_string(), &sample).is_err());
        sample.model_id = CURRENT_VOICE_MODEL;
        sample.embedding = &[f32::NAN, 1.0];
        assert!(st.add_voice_sample(&uuid::Uuid::new_v4().to_string(), &sample).is_err());
        sample.embedding = &[1.0];
        assert!(st.add_voice_sample(&uuid::Uuid::new_v4().to_string(), &sample).is_err());
        assert_eq!(st.list_voice_samples(&p.id).unwrap().len(), 1);
        assert!(st.list_compatible_speakers("other-model").unwrap().is_empty());
        assert!(st.add_voice_sample("../escape", &sample).is_err());
    }

    #[test]
    fn profiles_preserve_names_and_cascade_samples() {
        let st = Store::open_in_memory().unwrap();
        assert!(st.create_voice_profile("Speaker 1").is_err());
        let p = st.create_voice_profile("  Ann  ").unwrap();
        assert_eq!(p.name, "Ann");
        assert!(st.create_voice_profile("ann").is_err());
        add(&st, &p.id, &[1.0]);
        assert_eq!(st.rename_voice_profile(&p.id, "Anne").unwrap().sample_count, 1);
        st.delete_speaker(&p.id).unwrap();
        assert!(st.list_voice_profiles().unwrap().is_empty());
        assert_eq!(st.conn.lock().query_row("SELECT COUNT(*) FROM voice_samples", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    }
}
