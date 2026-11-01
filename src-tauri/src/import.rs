//! Import an existing audio/video file as a meeting: ffmpeg converts it to
//! `mix.wav`, then the normal post-call pipeline runs.

use crate::pipeline;
use crate::state::AppState;
use hark_capture::video::run_ffmpeg;
use hark_share::subtitles::{parse_subtitles, SubtitleFormat, MAX_SUBTITLE_BYTES};
use hark_store::{Meeting, MeetingImport, MeetingSpeaker, MeetingStatus, NewSegment, Segment, Store};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use tauri::{AppHandle, Manager};

/// Title for an imported file: the file stem with separators turned into spaces.
pub fn title_from_path(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let t: String = stem.chars().map(|c| if c == '_' || c == '-' || c == '.' { ' ' } else { c }).collect();
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.is_empty() { "Imported recording".into() } else { t }
}

/// Arguments that turn any ffmpeg-readable input into 16 kHz mono PCM.
pub fn ffmpeg_audio_args(input: &Path, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(), "-loglevel".into(), "error".into(), "-y".into(),
        "-i".into(), input.to_string_lossy().into_owned(),
        "-vn".into(), "-ac".into(), "1".into(), "-ar".into(), "16000".into(), "-c:a".into(), "pcm_s16le".into(),
        out.to_string_lossy().into_owned(),
    ]
}

/// Copy the video stream (no transcode) into an mp4 the player can show.
pub fn ffmpeg_video_args(input: &Path, out: &Path) -> Vec<String> {
    vec![
        "-hide_banner".into(), "-loglevel".into(), "error".into(), "-y".into(),
        "-i".into(), input.to_string_lossy().into_owned(),
        "-map".into(), "0:v:0".into(), "-map".into(), "0:a:0?".into(),
        "-c:v".into(), "copy".into(), "-c:a".into(), "aac".into(), "-movflags".into(), "+faststart".into(),
        out.to_string_lossy().into_owned(),
    ]
}

fn has_video_stream(ffmpeg: &Path, input: &Path) -> bool {
    // `ffmpeg -i` with no output exits 1 but prints the stream list to stderr.
    let Ok(out) = Command::new(ffmpeg).args(["-hide_banner", "-i"]).arg(input).stdin(Stdio::null()).output() else { return false };
    let err = String::from_utf8_lossy(&out.stderr);
    err.lines().any(|l| l.contains("Video:") && !l.contains("attached pic"))
}

fn wav_duration_ms(path: &Path) -> Option<i64> {
    let r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();
    let frames = r.len() as i64 / spec.channels.max(1) as i64;
    Some(frames * 1000 / spec.sample_rate.max(1) as i64)
}

fn load_transcript(path: &Path) -> Result<Vec<NewSegment>, String> {
    let format = match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("srt") => SubtitleFormat::Srt,
        Some("vtt") => SubtitleFormat::Vtt,
        _ => return Err("choose an SRT or WebVTT transcript".into()),
    };
    let mut text = String::new();
    std::fs::File::open(path).map_err(|e| format!("could not open transcript: {e}"))?
        .take(MAX_SUBTITLE_BYTES as u64 + 1).read_to_string(&mut text)
        .map_err(|e| format!("could not read UTF-8 transcript: {e}"))?;
    parse_subtitles(&text,format).map_err(|e| e.to_string())
}

fn validate_transcript_duration(segments: &[NewSegment], duration_ms: i64) -> Result<(),String> {
    if segments.iter().any(|s| s.end_ms > duration_ms) {
        return Err("transcript cue extends beyond the media duration; check that the files belong together".into());
    }
    Ok(())
}

/// Import optional existing subtitles without starting inference, agents or
/// webhooks. Media-only imports retain the normal processing behavior.
pub fn import(app: &AppHandle, path: &Path, title: Option<&str>, transcript: Option<&Path>) -> Result<Meeting, String> {
    let state = app.state::<AppState>();
    let ffmpeg = state.ffmpeg.clone().ok_or("ffmpeg is missing from this install, so files cannot be imported")?;
    let meeting = import_into_store(&state.store,&ffmpeg,path,title,transcript)?;
    log::info!("import: {} -> meeting {} ({} ms, video={})", path.display(), meeting.id, meeting.duration_ms, meeting.has_video);
    if meeting.status == MeetingStatus::Processing {
        let app2 = app.clone();
        let m2 = meeting.clone();
        std::thread::spawn(move || pipeline::post_process(&app2, m2));
    }
    Ok(meeting)
}

fn import_into_store(store: &Store, ffmpeg: &Path, path: &Path, title: Option<&str>, transcript: Option<&Path>) -> Result<Meeting,String> {
    if !path.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    let transcript = transcript.map(load_transcript).transpose()?;
    let title = title.map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| title_from_path(path));
    let mut meeting = Meeting::new_recording(&title, Some("import"));
    meeting.status = MeetingStatus::Processing;
    meeting.title_auto = false; // the file name is the user's title until they change it
    if let Ok(md) = std::fs::metadata(path) {
        if let Ok(modified) = md.modified() {
            meeting.started_at = modified.into();
        }
    }
    let stage = hark_share::staging::ImportStage::new(store).map_err(|e|e.to_string())?;
    let dir = stage.path();
    let mix = dir.join("mix.wav");
    if let Err(e) = run_ffmpeg(ffmpeg, &ffmpeg_audio_args(path, &mix)) {
        return Err(format!("could not read {}: {}", path.display(), if e.is_empty() { "unsupported format".to_string() } else { e }));
    }
    let duration = wav_duration_ms(&mix).unwrap_or(0);
    if duration < 500 {
        return Err("the file has no audio".into());
    }
    meeting.duration_ms = duration;
    meeting.ended_at = Some(meeting.started_at + chrono::Duration::milliseconds(duration));
    if has_video_stream(ffmpeg, path) {
        // fable: stream copy only - a codec the webview cannot play just means audio-only playback.
        match run_ffmpeg(ffmpeg, &ffmpeg_video_args(path, &dir.join("screen.mp4"))) {
            Ok(()) => meeting.has_video = true,
            Err(e) => {
                log::warn!("import: video copy failed, audio only: {e}");
                let _ = std::fs::remove_file(dir.join("screen.mp4"));
            }
        }
    }
    let mut segments = Vec::new();
    let mut speakers = Vec::new();
    if let Some(subtitles) = transcript {
        validate_transcript_duration(&subtitles,duration)?;
        meeting.status = MeetingStatus::Ready;
        for s in subtitles {
            if let Some(label) = &s.speaker {
                if !speakers.iter().any(|r: &MeetingSpeaker| &r.label == label) {
                    speakers.push(MeetingSpeaker { meeting_id:meeting.id.clone(),label:label.clone(),speaker_id:None,suggested_id:None,suggested_name:None,suggested_score:None });
                }
            }
            segments.push(Segment { id:0,meeting_id:meeting.id.clone(),start_ms:s.start_ms,end_ms:s.end_ms,speaker:s.speaker,text:s.text,clean_text:None });
        }
    }
    store.import_meeting(&MeetingImport {
        meeting:&meeting,segments:&segments,speakers:&speakers,summary:None,highlights:&[],tags:&[],transcript:None,
    },stage.path()).map_err(|e| e.to_string())?;
    Ok(meeting)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_come_from_the_file_name() {
        assert_eq!(title_from_path(Path::new("C:/x/2026-09-15_team-sync.mp3")), "2026 09 15 team sync");
        assert_eq!(title_from_path(Path::new("Voice 010.m4a")), "Voice 010");
        assert_eq!(title_from_path(Path::new("")), "Imported recording");
    }

    #[test]
    fn audio_args_force_16k_mono_pcm() {
        let a = ffmpeg_audio_args(Path::new("in.mp4"), Path::new("out.wav"));
        assert!(a.windows(2).any(|w| w == ["-ar", "16000"]));
        assert!(a.windows(2).any(|w| w == ["-ac", "1"]));
        assert!(a.contains(&"-vn".to_string()));
        assert_eq!(a.last().unwrap(), "out.wav");
    }

    #[test]
    fn subtitle_duration_must_fit_media_without_clamping_text() {
        let segment = NewSegment { start_ms:0,end_ms:2000,speaker:None,text:"original".into() };
        assert!(validate_transcript_duration(std::slice::from_ref(&segment),2000).is_ok());
        assert!(validate_transcript_duration(&[segment],1999).is_err());
    }

    #[test]
    #[ignore = "requires HARK_TEST_FFMPEG; generates synthetic media, no capture"]
    fn media_and_transcript_import_survives_library_reopen_without_inference() {
        let ffmpeg = std::env::var_os("HARK_TEST_FFMPEG").expect("set HARK_TEST_FFMPEG");
        let root = tempfile::tempdir().unwrap();
        let input = root.path().join("sample.wav");
        let mut writer = hound::WavWriter::create(&input,hound::WavSpec {
            channels:1,sample_rate:16000,bits_per_sample:16,sample_format:hound::SampleFormat::Int,
        }).unwrap();
        for _ in 0..16000 { writer.write_sample(0i16).unwrap(); }
        writer.finalize().unwrap();
        let transcript = root.path().join("sample.vtt");
        std::fs::write(&transcript,"WEBVTT\n\n00:00.000 --> 00:00.500\n<v Ann>um original text</v>\n").unwrap();
        let db = root.path().join("library/hark.db");
        let store = Store::open(&db).unwrap();
        let meeting = import_into_store(&store,Path::new(&ffmpeg),&input,None,Some(&transcript)).unwrap();
        assert_eq!(meeting.status,MeetingStatus::Ready);
        assert_eq!(meeting.duration_ms,1000);
        assert!(store.recordings_dir(&meeting.id).join("mix.wav").is_file());
        drop(store);
        let store = Store::open(&db).unwrap();
        let segments = store.segments(&meeting.id).unwrap();
        assert_eq!(segments[0].text,"um original text");
        assert!(segments[0].clean_text.is_none());
        assert_eq!(segments[0].speaker.as_deref(),Some("Ann"));
        assert_eq!(store.meeting_speakers(&meeting.id).unwrap()[0].label,"Ann");
        assert!(store.get_summary(&meeting.id).unwrap().is_none());
        assert_eq!(store.search_chunks("original",None,5).unwrap().len(),1);
        assert!(store.list_speakers().unwrap().is_empty());
    }
}
