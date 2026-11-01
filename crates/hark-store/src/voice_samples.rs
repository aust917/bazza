//! Managed local voice samples. Audio stays outside meeting exports.

use crate::{Result, Speaker, Store, StoreError};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// Identity of the sole voice embedding artifact in the current model catalog.
/// A backend/artifact change must use another identity, even at the same dimension.
pub const CURRENT_VOICE_MODEL: &str = "titanet-small:sha256:ad4a1802485d8b34c722d2a9d04249662f2ece5d28a7a039063ca22f515a789e";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoiceProfile {
    pub id: String,
    pub name: String,
    pub model_id: String,
    pub sample_count: i64,
    pub has_voiceprint: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VoiceSample {
    pub id: String,
    pub speaker_id: String,
    pub model_id: String,
    pub source: String,
    pub source_label: String,
    pub meeting_id: Option<String>,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub has_audio: bool,
    pub created_at: String,
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
    conn.execute("UPDATE meeting_speakers SET suggested_id = NULL, suggested_score = NULL WHERE suggested_id = ?1", params![id])?;
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
