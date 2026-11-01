//! `.hark` bundles: a zip with `manifest.json` plus each meeting's media files.
//! Import merges by meeting id, so re-importing is safe.

use crate::{Result, ShareError};
use hark_store::{Meeting, MeetingImport, MeetingSpeaker, MeetingStatus, Segment, Store, Summary};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;

pub const BUNDLE_VERSION: u32 = 1;
const MEDIA_FILES: &[&str] = &["mix.wav", "mic.wav", "sys.wav", "screen.mp4"];
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_MEDIA_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024 * 1024;
const MAX_MEETINGS: usize = 1000;
const MAX_ZIP_ENTRIES: usize = MAX_MEETINGS * 4 + 1;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BundleMeeting {
    pub meeting: Meeting,
    pub segments: Vec<Segment>,
    pub speakers: Vec<MeetingSpeaker>,
    pub summary: Option<Summary>,
    pub highlights: Vec<u64>,
    pub tags: Vec<String>,
    /// File names inside `media/<meeting-id>/`.
    pub media: Vec<String>,
    /// Additive v1 extension; credentials and arbitrary settings are excluded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<serde_json::Value>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BundleManifest {
    pub version: u32,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    pub app: String,
    pub meetings: Vec<BundleMeeting>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct ImportReport {
    pub imported: usize,
    pub skipped_existing: usize,
    pub errors: Vec<String>,
}

fn collect(store: &Store, meeting_id: &str) -> Result<BundleMeeting> {
    validate_id(meeting_id)?;
    let meeting = store.get_meeting(meeting_id)?.ok_or_else(|| ShareError::Other(format!("meeting {meeting_id} not found")))?;
    validate_finished(&meeting)?;
    Ok(BundleMeeting {
        segments: store.segments(meeting_id)?,
        speakers: store.meeting_speakers(meeting_id)?,
        summary: store.get_summary(meeting_id)?,
        highlights: store.get_setting(&format!("highlights:{meeting_id}"))?.unwrap_or_default(),
        tags: store.meeting_tags(meeting_id)?.into_iter().map(|t| t.name).collect(),
        media: MEDIA_FILES.iter().filter(|f| store.recordings_dir(meeting_id).join(f).exists()).map(|f| f.to_string()).collect(),
        transcript: store.get_setting::<serde_json::Value>(&format!("stt:{meeting_id}"))?.map(safe_transcript).transpose()?,
        meeting,
    })
}

/// Write a bundle with the given meetings. `include_video` false drops `screen.mp4`.
pub fn export_bundle(store: &Store, meeting_ids: &[String], out: &Path, include_video: bool) -> Result<BundleManifest> {
    let mut meetings = Vec::new();
    for id in meeting_ids {
        let mut m = collect(store, id)?;
        if !m.media.iter().any(|name| name == "mix.wav") {
            return Err(invalid(format!("meeting {id} has no mix.wav to export")));
        }
        if !include_video {
            m.media.retain(|f| f != "screen.mp4");
        }
        m.meeting.has_video = m.media.iter().any(|f| f == "screen.mp4");
        meetings.push(m);
    }
    let manifest = BundleManifest { version: BUNDLE_VERSION, exported_at: chrono::Utc::now(), app: "hark".into(), meetings };
    let file = File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let deflate = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("manifest.json", deflate)?;
    zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    for m in &manifest.meetings {
        let dir = store.recordings_dir(&m.meeting.id);
        for f in &m.media {
            let p = dir.join(f);
            // Media is already compressed (or huge); store without deflate to keep export fast.
            let mut src = File::open(&p)?;
            zip.start_file(format!("media/{}/{}", m.meeting.id, f), stored.large_file(src.metadata()?.len() > u32::MAX as u64))?;
            std::io::copy(&mut src, &mut zip)?;
        }
    }
    zip.finish()?;
    Ok(manifest)
}

/// Read only the manifest (for a preview before importing).
pub fn read_manifest(bundle: &Path) -> Result<BundleManifest> {
    let mut zip = open_bundle(bundle)?;
    read_validated_manifest(&mut zip)
}

fn invalid(message: impl Into<String>) -> ShareError {
    ShareError::Other(message.into())
}

fn validate_finished(meeting: &Meeting) -> Result<()> {
    if matches!(meeting.status,MeetingStatus::Recording|MeetingStatus::Processing) {
        return Err(invalid("finish recording and processing before exporting/importing a meeting package"));
    }
    Ok(())
}

/// Copy only the stable provider transcript contract, never arbitrary settings
/// or credentials even if an old/custom setting contains additional fields.
fn safe_transcript(value: serde_json::Value) -> Result<serde_json::Value> {
    let object = value.as_object().ok_or_else(||invalid("transcript provenance must be an object"))?;
    let provider = object.get("provider").and_then(|v|v.as_str()).filter(|s|matches!(*s,"local"|"soniox"))
        .ok_or_else(||invalid("unsupported transcript provider"))?;
    let status = object.get("status").and_then(|v|v.as_str()).filter(|s|matches!(*s,"streaming"|"complete"|"incomplete"))
        .ok_or_else(||invalid("invalid transcript status"))?;
    let model = object.get("model").and_then(|v|v.as_str()).filter(|s|!s.is_empty() && s.len()<=200)
        .ok_or_else(||invalid("invalid transcript model"))?;
    let optional_text = |key: &str,limit: usize| -> Result<serde_json::Value> {
        match object.get(key) {
            None | Some(serde_json::Value::Null) => Ok(serde_json::Value::Null),
            Some(serde_json::Value::String(s)) if s.len()<=limit => Ok(serde_json::Value::String(s.clone())),
            _ => Err(invalid(format!("invalid transcript {key}"))),
        }
    };
    let captions = object.get("captions").and_then(|v|v.as_array()).filter(|c|c.len()<=100_000)
        .ok_or_else(||invalid("invalid transcript captions"))?;
    let mut safe = Vec::with_capacity(captions.len());
    let mut previous = 0;
    for c in captions {
        let start = c.get("start_ms").and_then(|v|v.as_i64()).ok_or_else(||invalid("invalid caption start"))?;
        let end = c.get("end_ms").and_then(|v|v.as_i64()).ok_or_else(||invalid("invalid caption end"))?;
        let text = c.get("text").and_then(|v|v.as_str()).filter(|t|t.len()<=1024*1024).ok_or_else(||invalid("invalid caption text"))?;
        let is_final = c.get("is_final").and_then(|v|v.as_bool()).ok_or_else(||invalid("invalid caption final state"))?;
        if start<previous || end<start || !is_final { return Err(invalid("selected transcript must contain chronological final captions")); }
        previous=start;
        safe.push(serde_json::json!({"start_ms":start,"end_ms":end,"text":text,"is_final":is_final}));
    }
    Ok(serde_json::json!({"provider":provider,"model":model,"language":optional_text("language",64)?,
        "status":status,"error":optional_text("error",1000)?,"captions":safe}))
}

fn validate_id(id: &str) -> Result<()> {
    if id.len() != 36 || !id.bytes().enumerate().all(|(i, b)| {
        if [8, 13, 18, 23].contains(&i) { b == b'-' } else { b.is_ascii_digit() || (b'a'..=b'f').contains(&b) }
    }) {
        return Err(invalid("meeting ID must be a canonical UUID"));
    }
    Ok(())
}

// Bound the entry count before zip constructs its central-directory index.
// The bounded EOCD tail and optional fixed ZIP64 record are not media payloads.
fn preflight_directory(reader: &mut File) -> Result<()> {
    let length = reader.metadata()?.len();
    let tail_length = length.min(65_557) as usize;
    if tail_length < 22 { return Err(invalid("bundle is not a ZIP archive")); }
    reader.seek(SeekFrom::Start(length-tail_length as u64))?;
    let mut tail = vec![0u8;tail_length];
    reader.read_exact(&mut tail)?;
    let index = (0..=tail.len()-22).rev().find(|i| {
        &tail[*i..*i+4] == b"PK\x05\x06"
            && *i+22+u16::from_le_bytes([tail[*i+20],tail[*i+21]]) as usize == tail.len()
    }).ok_or_else(||invalid("bundle has no valid ZIP directory footer"))?;
    let footer = &tail[index..];
    let u16_at = |offset| u16::from_le_bytes([footer[offset],footer[offset+1]]);
    if u16_at(4) != 0 || u16_at(6) != 0 || u16_at(8) != u16_at(10) {
        return Err(invalid("multi-disk bundles are not supported"));
    }
    let mut count = u16_at(10) as u64;
    let footer_position = length-tail.len() as u64+index as u64;
    if footer_position >= 20 {
        reader.seek(SeekFrom::Start(footer_position-20))?;
        let mut locator = [0u8;20];
        reader.read_exact(&mut locator)?;
        if &locator[..4] == b"PK\x06\x07" {
            let disk = u32::from_le_bytes(locator[4..8].try_into().unwrap());
            let record_position = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            let disks = u32::from_le_bytes(locator[16..20].try_into().unwrap());
            if disk != 0 || disks != 1 || record_position > length.saturating_sub(56) {
                return Err(invalid("invalid ZIP64 directory locator"));
            }
            reader.seek(SeekFrom::Start(record_position))?;
            let mut record = [0u8;56];
            reader.read_exact(&mut record)?;
            if &record[..4] != b"PK\x06\x06" || record[16..24] != [0;8] {
                return Err(invalid("invalid ZIP64 directory"));
            }
            let disk_count = u64::from_le_bytes(record[24..32].try_into().unwrap());
            count = u64::from_le_bytes(record[32..40].try_into().unwrap());
            if disk_count != count { return Err(invalid("multi-disk ZIP64 bundles are not supported")); }
        }
    }
    if count > MAX_ZIP_ENTRIES as u64 { return Err(invalid("bundle contains too many entries")); }
    Ok(())
}

fn open_bundle(bundle: &Path) -> Result<zip::ZipArchive<File>> {
    let mut file = File::open(bundle)?;
    preflight_directory(&mut file)?;
    let mut directory_reader = file.try_clone()?;
    let zip = zip::ZipArchive::new(file)?;
    validate_raw_directory(&mut directory_reader,&zip)?;
    Ok(zip)
}

// zip 2 indexes by name, which hides duplicate names before by_index can see
// them. Inspect the bounded central-directory records before reading payloads.
fn validate_raw_directory(reader: &mut File, zip: &zip::ZipArchive<File>) -> Result<()> {
    reader.seek(SeekFrom::Start(zip.central_directory_start()))?;
    let mut names = HashSet::new();
    loop {
        let mut header = [0u8;46];
        reader.read_exact(&mut header[..4])?;
        if &header[..4] != b"PK\x01\x02" { break; }
        reader.read_exact(&mut header[4..])?;
        let length = |offset| u16::from_le_bytes([header[offset],header[offset+1]]) as usize;
        let name_len = length(28);
        if name_len > 128 || names.len() >= MAX_ZIP_ENTRIES { return Err(invalid("bundle directory exceeds entry limits")); }
        let mut name = vec![0;name_len];
        reader.read_exact(&mut name)?;
        if !names.insert(name) { return Err(invalid("bundle contains duplicate ZIP entries")); }
        reader.seek(SeekFrom::Current((length(30)+length(32)) as i64))?;
    }
    if names.len() != zip.len() { return Err(invalid("inconsistent or duplicate ZIP entries")); }
    Ok(())
}

fn read_validated_manifest(zip: &mut zip::ZipArchive<File>) -> Result<BundleManifest> {
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(invalid("bundle contains too many entries"));
    }
    let mut names = HashSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        let entry = zip.by_index(i)?;
        if !names.insert(entry.name().to_string()) {
            return Err(invalid("bundle contains duplicate ZIP entries"));
        }
        if entry.is_dir() || entry.unix_mode().is_some_and(|mode| mode & 0o170000 == 0o120000) {
            return Err(invalid("bundle entries must be regular files"));
        }
        if entry.size() > MAX_MEDIA_BYTES {
            return Err(invalid("bundle media exceeds the 16 GiB per-file limit"));
        }
        total = total.checked_add(entry.size()).ok_or_else(|| invalid("bundle size overflow"))?;
        if total > MAX_TOTAL_BYTES {
            return Err(invalid("bundle exceeds the 64 GiB uncompressed limit"));
        }
    }
    let mut f = zip.by_name("manifest.json")?;
    if f.size() > MAX_MANIFEST_BYTES {
        return Err(invalid("bundle manifest exceeds 16 MiB"));
    }
    let mut s = String::new();
    (&mut f).take(MAX_MANIFEST_BYTES + 1).read_to_string(&mut s)?;
    if s.len() as u64 > MAX_MANIFEST_BYTES { return Err(invalid("bundle manifest exceeds 16 MiB")); }
    let mut manifest: BundleManifest = serde_json::from_str(&s)?;
    drop(f);
    if manifest.version != BUNDLE_VERSION {
        return Err(invalid(format!("unsupported bundle version {}; supported version is {BUNDLE_VERSION}",manifest.version)));
    }
    if manifest.meetings.len() > MAX_MEETINGS {
        return Err(invalid("bundle contains too many meetings"));
    }
    let mut ids = HashSet::new();
    let mut expected = HashSet::from(["manifest.json".to_string()]);
    for bm in &mut manifest.meetings {
        bm.transcript = bm.transcript.take().map(safe_transcript).transpose()?;
        validate_id(&bm.meeting.id)?;
        validate_finished(&bm.meeting)?;
        let id = &bm.meeting.id;
        if !ids.insert(id) { return Err(invalid("bundle contains duplicate meeting IDs")); }
        if bm.meeting.duration_ms < 0 { return Err(invalid("meeting duration cannot be negative")); }
        if !bm.media.iter().any(|f| f == "mix.wav") {
            return Err(invalid(format!("meeting {id} has no mix.wav")));
        }
        let mut media = HashSet::new();
        for name in &bm.media {
            if !MEDIA_FILES.contains(&name.as_str()) || !media.insert(name) {
                return Err(invalid(format!("meeting {id} contains an invalid or duplicate media name")));
            }
            let path = format!("media/{id}/{name}");
            if !names.contains(&path) { return Err(invalid(format!("bundle is missing {path}"))); }
            if zip.by_name(&path)?.size() == 0 { return Err(invalid(format!("bundle media {path} is empty"))); }
            expected.insert(path);
        }
        if bm.segments.len() > 100_000 { return Err(invalid("meeting contains too many transcript segments")); }
        for segment in &bm.segments {
            if segment.meeting_id != *id || segment.start_ms < 0 || segment.end_ms < segment.start_ms {
                return Err(invalid(format!("meeting {id} contains invalid transcript timestamps or IDs")));
            }
        }
        let mut labels = HashSet::new();
        if bm.speakers.iter().any(|s| s.meeting_id != *id || !labels.insert(&s.label)) {
            return Err(invalid("invalid or duplicate meeting speaker labels"));
        }
        if bm.summary.as_ref().is_some_and(|s| s.meeting_id != *id) {
            return Err(invalid("summary belongs to another meeting"));
        }
    }
    if names != expected {
        return Err(invalid("bundle contains undeclared entries"));
    }
    Ok(manifest)
}

/// Import a bundle, skipping meetings that already exist.
pub fn import_bundle(store: &Store, bundle: &Path) -> Result<ImportReport> {
    let mut zip = open_bundle(bundle)?;
    let manifest = read_validated_manifest(&mut zip)?;
    let mut report = ImportReport::default();
    for bm in &manifest.meetings {
        let id = &bm.meeting.id;
        if store.get_meeting(id)?.is_some() {
            report.skipped_existing += 1;
            continue;
        }
        let res: Result<()> = (|| {
            let stage = crate::staging::ImportStage::new(store)?;
            for f in &bm.media {
                let name = format!("media/{id}/{f}");
                let mut entry = zip.by_name(&name)?;
                let size = entry.size();
                let mut out = File::create(stage.path().join(f))?;
                let copied = std::io::copy(&mut (&mut entry).take(size + 1), &mut out)?;
                if copied != size { return Err(invalid(format!("invalid media size for {name}"))); }
                out.sync_all()?;
            }
            let mut meeting = bm.meeting.clone();
            meeting.folder_id = None; // folders are local
            meeting.has_video = bm.media.iter().any(|f| f == "screen.mp4");
            let mut segments = bm.segments.clone();
            segments.sort_by_key(|s| s.start_ms);
            let inserted = store.import_meeting(&MeetingImport {
                meeting: &meeting, segments: &segments, speakers: &bm.speakers,
                summary: bm.summary.as_ref(), highlights: &bm.highlights, tags: &bm.tags, transcript:bm.transcript.as_ref(),
            }, stage.path())?;
            if inserted { report.imported += 1; } else { report.skipped_existing += 1; }
            Ok(())
        })();
        match res {
            Ok(()) => {},
            Err(e) => report.errors.push(format!("{}: {e}", bm.meeting.title)),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hark_store::NewSegment;

    #[test]
    fn bundle_roundtrip_merges_without_duplicates() {
        let src_dir = tempfile::tempdir().unwrap();
        let src = Store::open(&src_dir.path().join("hark.db")).unwrap();
        let mut m = Meeting::new_recording("Budget", Some("zoom"));
        m.status = MeetingStatus::Ready;
        m.duration_ms = 4000;
        src.create_meeting(&m).unwrap();
        src.replace_segments(&m.id, &[NewSegment { start_ms: 0, end_ms: 1000, speaker: Some("Sam".into()), text: "um hello".into() }]).unwrap();
        let sid = src.segments(&m.id).unwrap()[0].id;
        src.set_clean_text(&m.id, &[(sid, "Hello.".into())]).unwrap();
        src.tag_meeting(&m.id, "finance").unwrap();
        src.set_setting(&format!("highlights:{}", m.id), &vec![500u64]).unwrap();
        std::fs::write(src.recordings_dir(&m.id).join("mix.wav"), b"RIFFfake").unwrap();
        std::fs::write(src.recordings_dir(&m.id).join("screen.mp4"), b"video").unwrap();
        src.set_meeting_speakers(&m.id,&[MeetingSpeaker {
            meeting_id:m.id.clone(),label:"Sam".into(),speaker_id:Some("local-profile".into()),
            suggested_id:Some("local-suggestion".into()),suggested_name:None,suggested_score:Some(0.9),
        }]).unwrap();
        let summary = Summary { meeting_id:m.id.clone(),template_id:"notes".into(),markdown:"# Budget".into(),
            structured:serde_json::json!({"decisions":["save"]}),model:"local-model".into(),created_at:chrono::Utc::now() };
        src.set_summary(&summary).unwrap();
        let profile = src.create_voice_profile("Local voice").unwrap();
        src.add_voice_sample("11111111-1111-4111-8111-111111111111",&hark_store::NewVoiceSample {
            speaker_id:&profile.id,embedding:&[1.0,0.0],model_id:hark_store::CURRENT_VOICE_MODEL,
            source:"meeting",source_label:"private-reference-voice",meeting_id:Some(&m.id),
            start_ms:Some(0),end_ms:Some(1000),has_audio:true,
        }).unwrap();
        src.set_setting("private:soniox_api_key",&"synthetic-secret-not-for-export").unwrap();
        let provenance = serde_json::json!({
            "provider":"soniox","model":"stt-rt-v4","language":"ru","status":"incomplete","error":"network unavailable",
            "captions":[{"start_ms":0,"end_ms":1000,"text":"um hello","is_final":true}],
        });
        let mut unsafe_extra = provenance.clone();
        unsafe_extra["api_key"] = serde_json::json!("synthetic-key-inside-extra-field");
        unsafe_extra["captions"][0]["api_key"] = serde_json::json!("synthetic-caption-secret");
        src.set_setting(&format!("stt:{}",m.id),&unsafe_extra).unwrap();
        std::fs::write(src.recordings_dir(&m.id).join("voice-reference.wav"),b"private voice audio").unwrap();

        let out = src_dir.path().join("export.hark");
        let manifest = export_bundle(&src, &[m.id.clone()], &out, true).unwrap();
        assert_eq!(manifest.meetings[0].media, vec!["mix.wav","screen.mp4"]);
        let encoded = serde_json::to_string(&manifest).unwrap();
        assert!(!encoded.contains("private-reference-voice") && !encoded.contains("synthetic-secret-not-for-export") && !encoded.contains("embedding"));
        assert!(!encoded.contains("synthetic-key-inside-extra-field") && !encoded.contains("synthetic-caption-secret"));
        assert_eq!(manifest.meetings[0].transcript.as_ref(),Some(&provenance));
        let zip = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
        assert_eq!(zip.len(),3,"only manifest and selected media are exported");
        assert!(zip.file_names().all(|name| name == "manifest.json" || name.ends_with("/mix.wav") || name.ends_with("/screen.mp4")));

        let dst_dir = tempfile::tempdir().unwrap();
        let dst = Store::open(&dst_dir.path().join("hark.db")).unwrap();
        let r = import_bundle(&dst, &out).unwrap();
        assert_eq!(r, ImportReport { imported: 1, skipped_existing: 0, errors: vec![] });
        let got = dst.get_meeting(&m.id).unwrap().unwrap();
        assert_eq!(got.title, "Budget");
        assert!(got.has_video);
        let segs = dst.segments(&m.id).unwrap();
        assert_eq!(segs[0].text,"um hello");
        assert_eq!((segs[0].start_ms,segs[0].end_ms),(0,1000));
        assert_eq!(segs[0].clean_text.as_deref(), Some("Hello."));
        assert_eq!(dst.meeting_tags(&m.id).unwrap()[0].name, "finance");
        assert_eq!(std::fs::read(dst.recordings_dir(&m.id).join("mix.wav")).unwrap(), b"RIFFfake");
        assert_eq!(std::fs::read(dst.recordings_dir(&m.id).join("screen.mp4")).unwrap(), b"video");
        assert_eq!(dst.get_summary(&m.id).unwrap(),Some(summary));
        assert_eq!(dst.get_setting::<Vec<u64>>(&format!("highlights:{}",m.id)).unwrap(),Some(vec![500]));
        let speaker = dst.meeting_speakers(&m.id).unwrap().remove(0);
        assert_eq!(speaker.label,"Sam");
        assert!(speaker.speaker_id.is_none() && speaker.suggested_id.is_none() && speaker.suggested_score.is_none());
        assert!(dst.list_speakers().unwrap().is_empty());
        assert!(dst.get_setting::<String>("private:soniox_api_key").unwrap().is_none());
        assert_eq!(dst.get_setting::<serde_json::Value>(&format!("stt:{}",m.id)).unwrap(),Some(provenance));
        assert_eq!(dst.search_chunks("hello", None, 5).unwrap().len(), 1);

        let r2 = import_bundle(&dst, &out).unwrap();
        assert_eq!(r2.skipped_existing, 1);
        assert_eq!(dst.list_meetings().unwrap().len(), 1);
    }

    fn fixture() -> BundleManifest {
        let mut meeting = Meeting::new_recording("fixture",None);
        meeting.status = MeetingStatus::Ready;
        BundleManifest { version:1,exported_at:chrono::Utc::now(),app:"hark".into(),meetings:vec![BundleMeeting {
            meeting,segments:vec![],speakers:vec![],summary:None,highlights:vec![],tags:vec![],media:vec!["mix.wav".into()],transcript:None,
        }] }
    }

    fn write_fixture(path: &Path, manifest: &BundleManifest, entries: &[(&str,&[u8])]) {
        let mut writer = zip::ZipWriter::new(File::create(path).unwrap());
        writer.start_file("manifest.json",SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
        writer.write_all(&serde_json::to_vec(manifest).unwrap()).unwrap();
        for (name,bytes) in entries {
            writer.start_file(*name,SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn malformed_bundles_never_write_media_or_metadata() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("library/hark.db")).unwrap();
        for mode in 0..12 {
            let mut manifest = fixture();
            let original = manifest.meetings[0].meeting.id.clone();
            let mut name = format!("media/{original}/mix.wav");
            match mode {
                0 => manifest.version = 999,
                1 => manifest.meetings[0].meeting.id = "../../outside".into(),
                2 => manifest.meetings[0].media = vec!["../../outside.wav".into()],
                3 => name = format!("media/{original}/missing.wav"),
                4 => manifest.meetings.push(manifest.meetings[0].clone()),
                5 => manifest.meetings[0].media.push("mix.wav".into()),
                6 => manifest.meetings[0].segments.push(Segment {id:0,meeting_id:"other".into(),start_ms:0,end_ms:1,speaker:None,text:"bad".into(),clean_text:None}),
                7..=9 => {
                    let mut transcript = serde_json::json!({"provider":"soniox","model":"test","language":null,"status":"complete","error":null,
                        "captions":[{"start_ms":0,"end_ms":1000,"text":"hello","is_final":true}]});
                    match mode {
                        7 => transcript["captions"][0]["start_ms"] = serde_json::json!(-1),
                        8 => transcript["provider"] = serde_json::json!("unknown-provider"),
                        9 => transcript["captions"][0]["is_final"] = serde_json::json!(false),
                        _ => unreachable!(),
                    }
                    manifest.meetings[0].transcript=Some(transcript);
                }
                10 => manifest.meetings[0].meeting.status=MeetingStatus::Recording,
                11 => manifest.meetings[0].meeting.status=MeetingStatus::Processing,
                _ => unreachable!(),
            }
            let path = root.path().join(format!("{mode}.hark"));
            write_fixture(&path,&manifest,&[(&name,b"media")]);
            assert!(import_bundle(&store,&path).is_err(),"mode {mode}");
            assert!(store.list_meetings().unwrap().is_empty());
            assert!(!store.root().join("recordings").exists());
        }
        assert!(!root.path().join("outside.wav").exists());
    }

    #[test]
    fn per_meeting_failure_keeps_other_imports_and_is_retryable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("library/hark.db")).unwrap();
        let mut manifest = fixture();
        let first = manifest.meetings[0].meeting.id.clone();
        let second_meeting = fixture().meetings.remove(0);
        let second = second_meeting.meeting.id.clone();
        manifest.meetings.push(second_meeting);
        std::fs::write(store.recordings_dir(&first).join("mic.wav"),b"original").unwrap();
        let path = root.path().join("two.hark");
        write_fixture(&path,&manifest,&[
            (&format!("media/{first}/mix.wav"),b"one"),
            (&format!("media/{second}/mix.wav"),b"two"),
        ]);
        let report = import_bundle(&store,&path).unwrap();
        assert_eq!((report.imported,report.errors.len()),(1,1));
        assert!(store.get_meeting(&first).unwrap().is_none());
        assert!(store.get_meeting(&second).unwrap().is_some());
        assert_eq!(std::fs::read(store.recordings_dir(&first).join("mic.wav")).unwrap(),b"original");
        std::fs::write(store.recordings_dir(&first).join(".hark-import-pending"),&first).unwrap();
        let report = import_bundle(&store,&path).unwrap();
        assert_eq!((report.imported,report.skipped_existing),(1,1));
        assert!(report.errors.is_empty());
        assert_eq!(std::fs::read(store.recordings_dir(&first).join("mix.wav")).unwrap(),b"one");
        assert!(!std::fs::read_dir(store.root().join("recordings")).unwrap().any(|e|e.unwrap().file_name().to_string_lossy().starts_with(".hark-import-")));
    }

    #[test]
    fn export_without_video_reports_audio_only() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("hark.db")).unwrap();
        let mut meeting = Meeting::new_recording("audio",None);
        meeting.status = MeetingStatus::Ready;
        meeting.has_video = true;
        store.create_meeting(&meeting).unwrap();
        std::fs::write(store.recordings_dir(&meeting.id).join("mix.wav"),b"audio").unwrap();
        std::fs::write(store.recordings_dir(&meeting.id).join("screen.mp4"),b"video").unwrap();
        let manifest = export_bundle(&store,&[meeting.id],&root.path().join("audio.hark"),false).unwrap();
        assert!(!manifest.meetings[0].meeting.has_video);
        assert_eq!(manifest.meetings[0].media,vec!["mix.wav"]);
    }

    #[test]
    fn active_export_is_rejected_before_creating_output() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("hark.db")).unwrap();
        let mut meeting = Meeting::new_recording("active",None);
        store.create_meeting(&meeting).unwrap();
        let out = root.path().join("active.hark");
        assert!(export_bundle(&store,&[meeting.id.clone()],&out,true).unwrap_err().to_string().contains("finish"));
        assert!(!out.exists());
        meeting.status=MeetingStatus::Processing;
        store.update_meeting(&meeting).unwrap();
        assert!(export_bundle(&store,&[meeting.id.clone()],&out,true).is_err());
        meeting.status=MeetingStatus::Failed;
        store.update_meeting(&meeting).unwrap();
        std::fs::write(store.recordings_dir(&meeting.id).join("mix.wav"),b"partial audio").unwrap();
        assert!(export_bundle(&store,&[meeting.id],&out,true).is_ok());
    }

    #[test]
    fn corrupt_media_rolls_back_and_corrected_package_can_retry() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("library/hark.db")).unwrap();
        let manifest = fixture();
        let id = &manifest.meetings[0].meeting.id;
        let path = root.path().join("corrupt.hark");
        let name = format!("media/{id}/mix.wav");
        let payload = b"UNIQUE-media-payload-for-crc";
        write_fixture(&path,&manifest,&[(&name,payload)]);
        let mut bytes = std::fs::read(&path).unwrap();
        let position = bytes.windows(payload.len()).position(|w|w==payload).unwrap();
        bytes[position] ^= 0x01;
        std::fs::write(&path,bytes).unwrap();
        let report = import_bundle(&store,&path).unwrap();
        assert_eq!((report.imported,report.errors.len()),(0,1));
        assert!(store.get_meeting(id).unwrap().is_none());
        assert!(!store.root().join("recordings").join(id).exists());
        write_fixture(&path,&manifest,&[(&name,payload)]);
        assert_eq!(import_bundle(&store,&path).unwrap().imported,1);
    }

    #[test]
    fn duplicate_archive_entries_and_oversized_manifest_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("duplicate.hark");
        let manifest = fixture();
        // Rewrite a distinct same-length entry name in both local/central headers.
        // ZIP readers then see two manifest.json entries.
        write_fixture(&path,&manifest,&[("manifestxjson",b"{}")]);
        let mut bytes = std::fs::read(&path).unwrap();
        for i in 0..bytes.len()-12 {
            if &bytes[i..i+13] == b"manifestxjson" { bytes[i..i+13].copy_from_slice(b"manifest.json"); }
        }
        std::fs::write(&path,bytes).unwrap();
        assert!(read_manifest(&path).unwrap_err().to_string().contains("duplicate"));
        let path = root.path().join("large.hark");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer.start_file("manifest.json",SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
        writer.write_all(&vec![b' '; MAX_MANIFEST_BYTES as usize + 1]).unwrap();
        writer.finish().unwrap();
        assert!(read_manifest(&path).unwrap_err().to_string().contains("16 MiB"));
    }

    #[test]
    fn excessive_declared_entries_are_rejected_before_zip_index_allocation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("count.hark");
        let manifest = fixture();
        write_fixture(&path,&manifest,&[]);
        let mut bytes = std::fs::read(&path).unwrap();
        let footer = bytes.len()-22;
        let count = (MAX_ZIP_ENTRIES as u16+1).to_le_bytes();
        bytes[footer+8..footer+10].copy_from_slice(&count);
        bytes[footer+10..footer+12].copy_from_slice(&count);
        std::fs::write(&path,bytes).unwrap();
        assert!(read_manifest(&path).unwrap_err().to_string().contains("too many entries"));
    }

    #[test]
    fn zip64_directory_remains_compatible_with_bounded_preflight() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("zip64.hark");
        let manifest = fixture();
        write_fixture(&path,&manifest,&[(&format!("media/{}/mix.wav",manifest.meetings[0].meeting.id),b"audio")]);
        let bytes = std::fs::read(&path).unwrap();
        let footer = bytes.len()-22;
        let size = u32::from_le_bytes(bytes[footer+12..footer+16].try_into().unwrap()) as u64;
        let offset = u32::from_le_bytes(bytes[footer+16..footer+20].try_into().unwrap()) as u64;
        let mut extended = bytes[..footer].to_vec();
        extended.extend_from_slice(b"PK\x06\x06");
        extended.extend_from_slice(&44u64.to_le_bytes());
        extended.extend_from_slice(&45u16.to_le_bytes());
        extended.extend_from_slice(&45u16.to_le_bytes());
        extended.extend_from_slice(&[0u8;8]); // disk numbers
        extended.extend_from_slice(&2u64.to_le_bytes());
        extended.extend_from_slice(&2u64.to_le_bytes());
        extended.extend_from_slice(&size.to_le_bytes());
        extended.extend_from_slice(&offset.to_le_bytes());
        extended.extend_from_slice(b"PK\x06\x07");
        extended.extend_from_slice(&0u32.to_le_bytes());
        extended.extend_from_slice(&(footer as u64).to_le_bytes());
        extended.extend_from_slice(&1u32.to_le_bytes());
        let mut end = bytes[footer..].to_vec();
        end[8..20].fill(0xff);
        extended.extend_from_slice(&end);
        std::fs::write(&path,extended).unwrap();
        assert_eq!(read_manifest(&path).unwrap().meetings[0].meeting.id,manifest.meetings[0].meeting.id);
    }
}
