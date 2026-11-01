use super::{err, CmdResult};
use crate::pipeline;
use crate::state::AppState;
use crate::recorder::notice;
use hark_store::{MeetingSpeaker, NewVoiceSample, RenameOutcome, Speaker, VoiceProfile, VoiceSample, CURRENT_VOICE_MODEL};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};

fn stored_embedding(state: &AppState, meeting_id: &str, label: &str) -> Option<Vec<f32>> {
    let map: BTreeMap<String, Vec<f32>> = state.store.get_setting(&format!("speaker_embeddings:{meeting_id}")).ok().flatten()?;
    map.get(label).cloned()
}

#[tauri::command]
pub fn meeting_speakers(state: State<AppState>, id: String) -> CmdResult<Vec<MeetingSpeaker>> {
    state.store.meeting_speakers(&id).map_err(err)
}

/// Rename a label ("Speaker 2") to a person's name everywhere in this meeting.
/// Giving two labels the same name merges them into one person. The voiceprint
/// is stored so future meetings can suggest the name.
#[tauri::command]
pub fn rename_speaker(app: AppHandle, id: String, label: String, name: String) -> CmdResult<RenameOutcome> {
    let state = app.state::<AppState>();
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("name cannot be empty".into());
    }
    let emb = stored_embedding(&state, &id, &label);
    let outcome = state.store.rename_speaker(&id, &label, &name, emb.as_deref()).map_err(err)?;
    // Keep a voiceprint reachable under the new label for further renames. After a
    // merge that is the blended print of both voices, not just the one we moved.
    {
        let key = format!("speaker_embeddings:{id}");
        let mut map: BTreeMap<String, Vec<f32>> = state.store.get_setting(&key).ok().flatten().unwrap_or_default();
        let had = map.remove(&label);
        let blended = outcome.speaker.as_ref().map(|s| s.embedding.clone()).filter(|e| !e.is_empty()).or(had);
        if let Some(e) = blended {
            map.insert(outcome.name.clone(), e);
        }
        let _ = state.store.set_setting(&key, &map);
    }
    if let Some(from) = &outcome.merged_from {
        notice(
            &app,
            "info",
            format!(
                "Merged {from} into {} - {} segment{} moved.",
                outcome.name,
                outcome.moved_segments,
                if outcome.moved_segments == 1 { "" } else { "s" }
            ),
        );
    }
    // Retrieval chunks bake the speaker label into their text; rebuild them (and
    // their embeddings) so Ask Hark knows who the renamed voice is. An existing
    // summary names speakers too, so refresh it once the renames stop coming.
    let seq = {
        let mut m = state.rename_seq.lock();
        let e = m.entry(id.clone()).or_insert(0);
        *e += 1;
        *e
    };
    let app2 = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = pipeline::reembed(&app2, &id) {
            log::warn!("reembed after rename failed: {e}");
        }
        std::thread::sleep(std::time::Duration::from_secs(4));
        let state = app2.state::<AppState>();
        if state.rename_seq.lock().get(&id).copied() != Some(seq) {
            return; // a later rename owns the refresh
        }
        if let Ok(Some(existing)) = state.store.get_summary(&id) {
            crate::summary_stage::run_summary(&app2, &id, Some(&existing.template_id));
        }
    });
    Ok(outcome)
}

/// Accept the "Is this Sarah?" suggestion for a label.
#[tauri::command]
pub fn accept_speaker_suggestion(app: AppHandle, id: String, label: String) -> CmdResult<RenameOutcome> {
    let rows = app.state::<AppState>().store.meeting_speakers(&id).map_err(err)?;
    let row = rows.into_iter().find(|r| r.label == label).ok_or("unknown speaker label")?;
    let name = row.suggested_name.ok_or("no suggestion for this speaker")?;
    rename_speaker(app, id, label, name)
}

#[tauri::command]
pub fn list_known_speakers(state: State<AppState>) -> CmdResult<Vec<Speaker>> {
    let mut v = state.store.list_speakers().map_err(err)?;
    for s in &mut v {
        s.embedding.clear(); // not useful to the UI, keep payload small
    }
    Ok(v)
}

#[tauri::command]
pub fn delete_known_speaker(state: State<AppState>, id: String) -> CmdResult<()> {
    let _files = VOICE_FILES.lock();
    recover_sample_files(&state.store)?;
    let mut staged = Vec::new();
    for sample in state.store.list_voice_samples(&id).map_err(err)? {
        if let Some(audio) = stage_sample_audio(&state, &sample)? { staged.push(audio); }
    }
    state.store.delete_speaker(&id).map_err(err)?;
    for audio in &mut staged { audio.committed = true; }
    for audio in &staged { audio.cleanup()?; }
    Ok(())
}

#[tauri::command]
pub fn list_voice_profiles(state: State<AppState>) -> CmdResult<Vec<VoiceProfile>> {
    let _files = VOICE_FILES.lock();
    recover_sample_files(&state.store)?;
    state.store.list_voice_profiles().map_err(err)
}

#[tauri::command]
pub fn create_voice_profile(state: State<AppState>, name: String) -> CmdResult<VoiceProfile> {
    state.store.create_voice_profile(&name).map_err(err)
}

#[tauri::command]
pub fn rename_voice_profile(state: State<AppState>, id: String, name: String) -> CmdResult<VoiceProfile> {
    state.store.rename_voice_profile(&id, &name).map_err(err)
}

#[tauri::command]
pub fn list_voice_samples(state: State<AppState>, speaker_id: String) -> CmdResult<Vec<VoiceSample>> {
    state.store.list_voice_samples(&speaker_id).map_err(err)
}

#[tauri::command]
pub fn delete_voice_sample(state: State<AppState>, id: String) -> CmdResult<()> {
    let _files = VOICE_FILES.lock();
    recover_sample_files(&state.store)?;
    let sample = state.store.get_voice_sample(&id).map_err(err)?.ok_or("voice sample not found")?;
    let mut staged = stage_sample_audio(&state, &sample)?;
    state.store.delete_voice_sample(&id).map_err(err)?;
    if let Some(audio) = &mut staged { audio.committed = true; audio.cleanup()?; }
    Ok(())
}

#[tauri::command]
pub fn recompute_voice_profile(state: State<AppState>, id: String) -> CmdResult<VoiceProfile> {
    state.store.recompute_voice_profile(&id).map_err(err)
}

#[tauri::command]
pub fn voice_sample_audio(state: State<AppState>, id: String) -> CmdResult<String> {
    let _files = VOICE_FILES.lock();
    recover_sample_files(&state.store)?;
    let sample = state.store.get_voice_sample(&id).map_err(err)?.ok_or("voice sample not found")?;
    if !sample.has_audio { return Err("this saved voiceprint has no retained audio".into()); }
    let path = sample_audio_path(&state, &id)?;
    if !path.is_file() { return Err("voice sample audio is missing".into()); }
    Ok(path.to_string_lossy().into_owned())
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VoiceSampleSource {
    File { path: String, start_ms: i64, end_ms: i64 },
    Meeting { meeting_id: String, start_ms: i64, end_ms: i64 },
}

fn validate_sample_interval(start: i64, end: i64) -> CmdResult<()> {
    if start < 0 || !end.checked_sub(start).is_some_and(|d| (1000..=120_000).contains(&d)) {
        return Err("choose 1–120 seconds of one person's speech".into());
    }
    Ok(())
}

fn sample_audio_path(state: &AppState, id: &str) -> CmdResult<PathBuf> {
    let id = uuid::Uuid::parse_str(id).map_err(|_| "invalid voice sample id")?;
    Ok(state.store.root().join("voice-samples").join(format!("{id}.wav")))
}

static VOICE_FILES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
static ACTIVE_VOICE_SAMPLES: parking_lot::Mutex<std::collections::BTreeSet<String>> = parking_lot::Mutex::new(std::collections::BTreeSet::new());

struct PendingSampleFiles { id: String, pending: PathBuf, audio: PathBuf, committed: bool }
impl PendingSampleFiles {
    fn new(id: String, pending: PathBuf, audio: PathBuf) -> Self {
        ACTIVE_VOICE_SAMPLES.lock().insert(id.clone());
        Self { id, pending, audio, committed: false }
    }
}
impl Drop for PendingSampleFiles {
    fn drop(&mut self) {
        let _files = VOICE_FILES.lock();
        let _ = std::fs::remove_file(&self.pending);
        if !self.committed { let _ = std::fs::remove_file(&self.audio); }
        ACTIVE_VOICE_SAMPLES.lock().remove(&self.id);
    }
}

struct StagedSampleAudio { original: PathBuf, staged: PathBuf, committed: bool }
impl StagedSampleAudio {
    fn stage(original: PathBuf) -> CmdResult<Option<Self>> {
        let id = original.file_stem().and_then(|s| s.to_str()).ok_or("invalid voice sample file")?;
        let staged = original.with_file_name(format!(".deleting-{id}.wav"));
        match std::fs::rename(&original, &staged) {
            Ok(()) => Ok(Some(Self { original, staged, committed: false })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("could not stage voice sample removal: {e}")),
        }
    }
    fn cleanup(&self) -> CmdResult<()> {
        match std::fs::remove_file(&self.staged) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("voice sample removed from memory; audio cleanup will retry when Voices opens: {e}")),
        }
    }
}
impl Drop for StagedSampleAudio {
    fn drop(&mut self) {
        if !self.committed && !self.original.exists() {
            if let Err(e) = std::fs::rename(&self.staged, &self.original) { log::warn!("voice sample rollback: {e}"); }
        }
    }
}

fn stage_sample_audio(state: &AppState, sample: &VoiceSample) -> CmdResult<Option<StagedSampleAudio>> {
    if sample.has_audio { StagedSampleAudio::stage(sample_audio_path(state, &sample.id)?) } else { Ok(None) }
}

/// An interrupted delete restores audio while its row exists; after commit it
/// completes removal. Call under VOICE_FILES so live deletes cannot race recovery.
fn recover_sample_files(store: &hark_store::Store) -> CmdResult<()> {
    let dir = store.root().join("voice-samples");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(err(e)),
    };
    for entry in entries {
        let entry = entry.map_err(err)?;
        if !entry.file_type().map_err(err)?.is_file() { continue; }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".wav") else { continue };
        if let Some(id) = stem.strip_prefix(".pending-") {
            if uuid::Uuid::parse_str(id).is_ok() && !ACTIVE_VOICE_SAMPLES.lock().contains(id) { std::fs::remove_file(entry.path()).map_err(err)?; }
            continue;
        }
        let Some(id) = stem.strip_prefix(".deleting-") else {
            if uuid::Uuid::parse_str(stem).is_ok() && !ACTIVE_VOICE_SAMPLES.lock().contains(stem) && store.get_voice_sample(stem).map_err(err)?.is_none() {
                std::fs::remove_file(entry.path()).map_err(err)?;
            }
            continue;
        };
        let Ok(uuid) = uuid::Uuid::parse_str(id) else { continue };
        let original = dir.join(format!("{uuid}.wav"));
        if store.get_voice_sample(id).map_err(err)?.is_some() && !original.exists() {
            std::fs::rename(entry.path(), original).map_err(err)?;
        } else { std::fs::remove_file(entry.path()).map_err(err)?; }
    }
    Ok(())
}

fn local_command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.stdin(Stdio::null());
    #[cfg(windows)] {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

fn run_sample_process(mut cmd: Command, timeout: Duration) -> CmdResult<Output> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(err)?;
    let overflow = Arc::new(AtomicBool::new(false));
    let read = |pipe: Box<dyn Read + Send>, cap: usize, overflow: Arc<AtomicBool>| std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = pipe.take(cap as u64 + 1).read_to_end(&mut bytes);
        if bytes.len() > cap { overflow.store(true, Ordering::Release); }
        result.map(|_| bytes)
    });
    let stdout = read(Box::new(child.stdout.take().ok_or("sample process stdout missing")?), 1_048_576, overflow.clone());
    let stderr = read(Box::new(child.stderr.take().ok_or("sample process stderr missing")?), 65_536, overflow.clone());
    let started = Instant::now();
    let result = loop {
        if overflow.load(Ordering::Acquire) { break Err("sample process output exceeded its limit".to_string()); }
        if started.elapsed() >= timeout { break Err("voice sample processing timed out; try a shorter interval".to_string()); }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => break Err(err(e)),
        }
    };
    if result.is_err() { let _ = child.kill(); let _ = child.wait(); }
    let stdout = stdout.join().map_err(|_| "sample process stdout reader failed")?.map_err(err)?;
    let stderr = stderr.join().map_err(|_| "sample process stderr reader failed")?.map_err(err)?;
    if overflow.load(Ordering::Acquire) { return Err("sample process output exceeded its limit".into()); }
    Ok(Output { status: result?, stdout, stderr })
}

fn one_voice_embedding(out: hark_diarize::DiarizeOutput) -> CmdResult<Vec<f32>> {
    if let Some(e) = out.error { return Err(e); }
    // Every detected voice must have an embedding. A short second voice must not
    // silently disappear merely because the extractor omitted its embedding.
    let clusters: std::collections::BTreeSet<_> = out.turns.iter().map(|t| t.cluster).collect();
    if clusters.len() != 1 || out.embeddings.len() != 1 {
        return Err("the selected interval must contain one speaker; choose another interval".into());
    }
    let (cluster, embedding) = out.embeddings.into_iter().next().ok_or("no usable voice found")?;
    if !clusters.contains(&cluster) || embedding.is_empty() || embedding.len() > 4096 || embedding.len() != out.embedding_dim || !embedding.iter().all(|x| x.is_finite()) || embedding.iter().all(|x| x.abs() < 1e-9) {
        return Err("no usable voiceprint found in this interval".into());
    }
    Ok(embedding)
}

#[tauri::command]
pub async fn add_voice_sample(app: AppHandle, speaker_id: String, source: VoiceSampleSource) -> CmdResult<VoiceSample> {
    tauri::async_runtime::spawn_blocking(move || add_voice_sample_inner(&app, &speaker_id, source)).await.map_err(err)?
}

fn add_voice_sample_inner(app: &AppHandle, speaker_id: &str, source: VoiceSampleSource) -> CmdResult<VoiceSample> {
    let state = app.state::<AppState>();
    let _busy = state.busy_guard();
    let profile = state.store.list_voice_profiles().map_err(err)?.into_iter().find(|p| p.id == speaker_id).ok_or("voice profile not found")?;
    if profile.model_id != CURRENT_VOICE_MODEL { return Err("this profile uses another voice model; create a profile for the current model".into()); }
    let ffmpeg = state.ffmpeg.as_ref().ok_or("ffmpeg is missing from this install")?;
    let sidecar = state.diarize_bin.as_ref().ok_or("speaker recognition sidecar is missing")?;
    let (seg_model, emb_model) = state.diarize_models().ok_or("download speaker recognition models in Settings first")?;
    let (input, start, end, kind, source_label, meeting_id) = match source {
        VoiceSampleSource::File { path, start_ms, end_ms } => {
            let path = PathBuf::from(path);
            let label = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            (path, start_ms, end_ms, "file", label, None)
        },
        VoiceSampleSource::Meeting { meeting_id, start_ms, end_ms } => {
            if meeting_id.is_empty() || meeting_id.contains(['/', '\\', ':']) || meeting_id == "." || meeting_id == ".." { return Err("invalid meeting id".into()); }
            let m = state.store.get_meeting(&meeting_id).map_err(err)?.ok_or("meeting not found")?;
            if end_ms > m.duration_ms { return Err("voice sample extends past the recording".into()); }
            (state.store.recordings_dir(&meeting_id).join("mix.wav"), start_ms, end_ms, "meeting", m.title, Some(meeting_id))
        },
    };
    validate_sample_interval(start, end)?;
    if !input.is_file() { return Err("sample source is not a local file".into()); }
    let sample_id = uuid::Uuid::new_v4().to_string();
    let audio = sample_audio_path(&state, &sample_id)?;
    let dir = audio.parent().ok_or("invalid voice sample directory")?;
    std::fs::create_dir_all(dir).map_err(err)?;
    let pending = dir.join(format!(".pending-{sample_id}.wav"));
    let mut files = PendingSampleFiles::new(sample_id.clone(), pending.clone(), audio.clone());
    let result = (|| {
        let mut convert = local_command(ffmpeg);
        convert.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-protocol_whitelist", "file,pipe", "-ss"])
            .arg(format!("{:.3}", start as f64 / 1000.0)).arg("-i").arg(&input)
            .arg("-t").arg(format!("{:.3}", (end - start) as f64 / 1000.0))
            .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"]).arg(&pending);
        let output = run_sample_process(convert, Duration::from_secs(60))?;
        if !output.status.success() { return Err(format!("could not read sample audio: {}", String::from_utf8_lossy(&output.stderr).trim())); }
        let wav = hound::WavReader::open(&pending).map_err(err)?;
        let duration = wav.duration() as i64 * 1000 / wav.spec().sample_rate.max(1) as i64;
        drop(wav);
        if duration < 1000 { return Err("sample contains less than one second of audio".into()); }
        let mut recognize = local_command(sidecar);
        recognize.arg("--seg").arg(&seg_model).arg("--emb").arg(&emb_model)
            .arg("--wav").arg(&pending).args(["--max-embed-secs", "120", "--threads", "2"]);
        let output = run_sample_process(recognize, Duration::from_secs(300))?;
        if !output.status.success() { return Err(format!("speaker recognition failed: {}", String::from_utf8_lossy(&output.stderr).trim())); }
        let embedding = one_voice_embedding(serde_json::from_slice(&output.stdout).map_err(err)?)?;
        let _files = VOICE_FILES.lock();
        std::fs::rename(&pending, &audio).map_err(err)?;
        let actual_end = start.checked_add(duration).ok_or("sample timestamp exceeds the supported range")?;
        state.store.add_voice_sample(&sample_id, &NewVoiceSample { speaker_id, embedding: &embedding, model_id: CURRENT_VOICE_MODEL, source: kind, source_label: &source_label, meeting_id: meeting_id.as_deref(), start_ms: Some(start), end_ms: Some(actual_end), has_audio: true }).map_err(err)
    })();
    files.committed = result.is_ok();
    result
}

#[tauri::command]
pub fn rerun_cleanup(app: AppHandle, id: String) -> CmdResult<()> {
    let state = app.state::<AppState>();
    state.store.get_meeting(&id).map_err(err)?.ok_or("meeting not found")?;
    state.store.clear_clean_text(&id).map_err(err)?;
    let app2 = app.clone();
    std::thread::spawn(move || {
        pipeline::run_cleanup(&app2, &id);
        let _ = pipeline::reembed(&app2, &id);
    });
    Ok(())
}

/// Try an OpenAI-compatible endpoint; returns the model list on success.
#[tauri::command]
pub async fn test_llm_endpoint(url: String, api_key: Option<String>, model: String) -> CmdResult<Vec<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        let c = hark_llm::OpenAiCompat::new(&url, api_key.as_deref(), &model);
        c.list_models().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod voice_tests {
    use super::*;
    use hark_diarize::{DiarizeOutput, Turn};

    #[test]
    fn sample_intervals_reject_invalid_or_unbounded_input() {
        for (start, end) in [(-1, 2000), (0, 0), (2000, 1000), (0, 999), (0, 120001), (i64::MAX, 0), (i64::MIN, i64::MAX)] {
            assert!(validate_sample_interval(start, end).is_err(), "{start}..{end}");
        }
        assert!(validate_sample_interval(10_000, 12_000).is_ok());
    }

    #[test]
    fn sample_extraction_rejects_an_unembedded_second_speaker() {
        let one = DiarizeOutput { turns: vec![Turn { start_ms: 0, end_ms: 2000, cluster: 0 }], embeddings: BTreeMap::from([(0, vec![1.0, 0.0])]), embedding_dim: 2, error: None };
        assert_eq!(one_voice_embedding(one.clone()).unwrap(), vec![1.0, 0.0]);
        let mut multiple = one.clone();
        multiple.turns.push(Turn { start_ms: 2000, end_ms: 2100, cluster: 1 });
        assert!(one_voice_embedding(multiple).is_err());
        let mut invalid = one;
        invalid.embeddings.insert(0, vec![f32::NAN, 1.0]);
        assert!(one_voice_embedding(invalid).is_err());
        assert!(one_voice_embedding(DiarizeOutput::default()).is_err());
    }

    struct TestDir(PathBuf);
    impl TestDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("hark-voice-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }

    #[test]
    fn staged_audio_rolls_back_and_crash_recovery_follows_database_state() {
        let _files = VOICE_FILES.lock();
        let dir = TestDir::new();
        let store = hark_store::Store::open(&dir.0.join("hark.db")).unwrap();
        let profile = store.create_voice_profile("Ann").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        store.add_voice_sample(&id, &NewVoiceSample { speaker_id: &profile.id, embedding: &[1.0], model_id: CURRENT_VOICE_MODEL, source: "file", source_label: "test.wav", meeting_id: None, start_ms: Some(0), end_ms: Some(2000), has_audio: true }).unwrap();
        let audio = dir.0.join("voice-samples").join(format!("{id}.wav"));
        std::fs::create_dir_all(audio.parent().unwrap()).unwrap();
        std::fs::write(&audio, b"test audio").unwrap();
        { let _staged = StagedSampleAudio::stage(audio.clone()).unwrap().unwrap(); assert!(!audio.exists()); }
        assert_eq!(std::fs::read(&audio).unwrap(), b"test audio");
        let tomb = audio.with_file_name(format!(".deleting-{id}.wav"));
        std::fs::rename(&audio, &tomb).unwrap();
        recover_sample_files(&store).unwrap();
        assert!(audio.is_file());
        std::fs::rename(&audio, &tomb).unwrap();
        store.delete_voice_sample(&id).unwrap();
        recover_sample_files(&store).unwrap();
        assert!(!tomb.exists());
        let orphan = dir.0.join("voice-samples").join(format!("{}.wav", uuid::Uuid::new_v4()));
        let pending = dir.0.join("voice-samples").join(format!(".pending-{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&orphan, b"interrupted commit").unwrap();
        std::fs::write(&pending, b"interrupted conversion").unwrap();
        recover_sample_files(&store).unwrap();
        assert!(!orphan.exists());
        assert!(!pending.exists());
    }

    #[test]
    fn sample_process_fixture_child() {
        match std::env::var("HARK_VOICE_TEST_CHILD").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(5)),
            Ok("overflow") => { eprint!("{}", "x".repeat(70_000)); },
            _ => {},
        }
    }

    #[test]
    fn sample_processes_stop_on_timeout_and_excess_output() {
        let child_command = |mode: &str| {
            let mut cmd = local_command(&std::env::current_exe().unwrap());
            cmd.args(["--exact", "commands::speakers::voice_tests::sample_process_fixture_child", "--nocapture"]).env("HARK_VOICE_TEST_CHILD", mode);
            cmd
        };
        assert!(run_sample_process(child_command("sleep"), Duration::from_millis(100)).unwrap_err().contains("timed out"));
        assert!(run_sample_process(child_command("overflow"), Duration::from_secs(10)).unwrap_err().contains("output exceeded"));
    }
}
