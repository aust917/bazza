//! `.bazza` bundles: a zip with `manifest.json` plus each meeting's media files.
//! Import merges by meeting id, so re-importing is safe.

use crate::{Result, ShareError};
use bazza_store::{Meeting, MeetingImport, MeetingSpeaker, MeetingStatus, Segment, Store, Summary};
use bazza_domain::transcript_versions::TranscriptVersionArchive;
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
    /// Typed additive v1 history; it carries no worker or provider authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_versions: Option<TranscriptVersionArchive>,
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
        media: MEDIA_FILES.iter()
            .filter(|f| (**f != "screen.mp4" || meeting.has_video) && store.recordings_dir(meeting_id).join(f).exists())
            .map(|f| f.to_string()).collect(),
        transcript: store.get_setting::<serde_json::Value>(&format!("stt:{meeting_id}"))?.map(safe_transcript).transpose()?,
        transcript_versions: store.transcript_version_archive(meeting_id)?,
        meeting,
    })
}

/// Write a bundle with the given meetings. `include_video` false drops `screen.mp4`.
pub fn export_bundle(store: &Store, meeting_ids: &[String], out: &Path, include_video: bool) -> Result<BundleManifest> {
    validate_bundle_path(out)?;
    if meeting_ids.len() > MAX_MEETINGS { return Err(invalid("bundle contains too many meetings")); }
    let mut meetings = Vec::new();
    let mut metadata_bytes = 0_u64;
    for id in meeting_ids {
        let mut m = collect(store, id)?;
        if !m.media.iter().any(|name| name == "mix.wav") {
            return Err(invalid(format!("meeting {id} has no mix.wav to export")));
        }
        if !include_video {
            m.media.retain(|f| f != "screen.mp4");
        }
        m.meeting.has_video = m.media.iter().any(|f| f == "screen.mp4");
        metadata_bytes = metadata_bytes.saturating_add(serde_json::to_vec(&m)?.len() as u64);
        if metadata_bytes > MAX_MANIFEST_BYTES { return Err(invalid("bundle manifest exceeds 16 MiB")); }
        meetings.push(m);
    }
    let manifest = BundleManifest { version: BUNDLE_VERSION, exported_at: chrono::Utc::now(), app: "bazza".into(), meetings };
    for row in &manifest.meetings {
        if row.transcript_versions.as_ref().is_some_and(|archive| !archive.valid(&row.meeting.id,&row.segments)) {
            return Err(invalid("transcript history changed during export"));
        }
    }
    let encoded = serde_json::to_vec_pretty(&manifest)?;
    if encoded.len() as u64 > MAX_MANIFEST_BYTES { return Err(invalid("bundle manifest exceeds 16 MiB")); }
    let file = File::create(out)?;
    let mut zip = zip::ZipWriter::new(file);
    let deflate = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("manifest.json", deflate)?;
    zip.write_all(&encoded)?;
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

fn validate_bundle_path(path: &Path) -> Result<()> {
    if !path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("bazza")) {
        return Err(invalid("choose a .bazza bundle"));
    }
    Ok(())
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
    validate_bundle_path(bundle)?;
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
    if manifest.app != "bazza" {
        return Err(invalid("bundle was not exported by Bazza"));
    }
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
            segment.validate_manual_correction().map_err(|error| invalid(format!("meeting {id} contains an invalid manual transcript correction: {error}")))?;
            if segment.meeting_id != *id || segment.start_ms < 0 || segment.end_ms < segment.start_ms {
                return Err(invalid(format!("meeting {id} contains invalid transcript timestamps or IDs")));
            }
        }
        if bm.transcript_versions.as_ref().is_some_and(|archive| !archive.valid(id,&bm.segments)) {
            return Err(invalid(format!("meeting {id} contains invalid transcript version history")));
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
            let inserted = store.import_meeting_with_versions(&MeetingImport {
                meeting: &meeting, segments: &segments, speakers: &bm.speakers,
                summary: bm.summary.as_ref(), highlights: &bm.highlights, tags: &bm.tags, transcript:bm.transcript.as_ref(),
            }, stage.path(), bm.transcript_versions.as_ref())?;
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
    use bazza_store::NewSegment;

    fn history_meeting(store: &Store, full: bool) -> (Meeting,String,String) {
        use bazza_domain::transcript_versions::{NewTranscriptVersion,NewTranscriptVersionSegment,TranscriptParticipantSource,TranscriptSegmentProvenance,TranscriptSegmentSource,TranscriptVersionSource};
        let mut meeting = Meeting::new_recording("Version history fixture",Some("meet"));
        meeting.duration_ms = 2000;
        store.create_meeting(&meeting).unwrap();
        let initial = NewSegment { start_ms:0,end_ms:1000,speaker:Some("Alex".into()),text:"original bot words".into() };
        store.publish_initial_live_transcript(&meeting.id,"whisper-small",Some("ru"),&[initial.clone()]).unwrap();
        meeting.status = MeetingStatus::Processing;
        store.update_processing_state(&meeting).unwrap();
        let complete_id = Meeting::new_recording("version id",None).id;
        if full {
            let mut bot: NewTranscriptVersionSegment = initial.into();
            bot.provenance = Some(TranscriptSegmentProvenance { source:TranscriptSegmentSource::Bot,provider:Some(TranscriptParticipantSource::Meet),participant_id:Some("participant-fixture".into()),source_start_ms:0,source_end_ms:1000,uncertain:false });
            let system = NewTranscriptVersionSegment { segment:NewSegment { start_ms:1000,end_ms:2000,speaker:Some("System".into()),text:"presentation words".into() },playback:None,
                provenance:Some(TranscriptSegmentProvenance { source:TranscriptSegmentSource::System,provider:None,participant_id:None,source_start_ms:1000,source_end_ms:2000,uncertain:false }) };
            store.save_transcript_version(&meeting.id,&NewTranscriptVersion { id:complete_id.clone(),source:TranscriptVersionSource::Combined,model:"whisper-small".into(),language:Some("ru".into()),partial:false,segments:vec![bot,system] }).unwrap();
        }
        let partial_id = Meeting::new_recording("partial id",None).id;
        let partial = NewTranscriptVersionSegment { segment:NewSegment { start_ms:1500,end_ms:1800,speaker:Some("System".into()),text:"uncertain extra".into() },playback:None,
            provenance:Some(TranscriptSegmentProvenance { source:TranscriptSegmentSource::System,provider:None,participant_id:None,source_start_ms:1500,source_end_ms:1800,uncertain:true }) };
        store.save_transcript_version(&meeting.id,&NewTranscriptVersion { id:partial_id.clone(),source:TranscriptVersionSource::Combined,model:"whisper-small".into(),language:Some("ru".into()),partial:true,segments:vec![partial] }).unwrap();
        meeting.status = MeetingStatus::Ready;
        store.update_processing_state(&meeting).unwrap();
        let current = store.segments(&meeting.id).unwrap();
        store.set_transcript_correction(&meeting.id,current.last().unwrap().id,Some("user corrected words")).unwrap();
        std::fs::write(store.recordings_dir(&meeting.id).join("mix.wav"),b"RIFF fixture").unwrap();
        (meeting,complete_id,partial_id)
    }

    #[test]
    fn version_history_roundtrip_keeps_original_partial_provenance_and_current_corrections() {
        use bazza_domain::transcript_versions::TranscriptVersionSource;
        let src_root = tempfile::tempdir().unwrap();
        let source = Store::open(&src_root.path().join("bazza.db")).unwrap();
        let (full,full_id,partial_id) = history_meeting(&source,true);
        let (partial,_,_) = history_meeting(&source,false);
        source.set_setting("private:soniox_api_key",&"fixture-secret-excluded").unwrap();
        let bot_dir = source.recordings_dir(&full.id).join("attendee");
        std::fs::create_dir(&bot_dir).unwrap();
        std::fs::write(bot_dir.join("participant-001.wav"),b"bot audio excluded").unwrap();
        let bundle = src_root.path().join("versions.bazza");
        let manifest = export_bundle(&source,&[full.id.clone(),partial.id.clone()],&bundle,false).unwrap();
        assert!(manifest.meetings.iter().all(|row|row.transcript_versions.is_some()));
        assert_eq!(zip::ZipArchive::new(File::open(&bundle).unwrap()).unwrap().len(),3);
        let encoded = serde_json::to_string(&manifest).unwrap();
        assert!(!encoded.contains("fixture-secret-excluded") && !encoded.contains("job_id") && !encoded.contains("participant-001.wav"));
        let dst_root = tempfile::tempdir().unwrap();
        let db = dst_root.path().join("bazza.db");
        let destination = Store::open(&db).unwrap();
        let other = Meeting::new_recording("force different canonical ids",None);
        destination.create_meeting(&other).unwrap();
        destination.replace_segments(&other.id,&(0..8).map(|i|NewSegment { start_ms:i,end_ms:i+1,speaker:None,text:"unrelated".into() }).collect::<Vec<_>>()).unwrap();
        assert_eq!(import_bundle(&destination,&bundle).unwrap().imported,2);
        let listing = destination.transcript_versions(&full.id).unwrap();
        assert_eq!(listing.effective_id.as_deref(),Some(full_id.as_str()));
        assert!(listing.versions.iter().any(|row|row.id == partial_id && row.partial));
        let original = destination.transcript_version(&full.id,"original").unwrap().unwrap();
        assert_eq!(original[0].segment.text,"original bot words");
        assert!(original.iter().all(|row|row.segment.id < 0));
        assert!(destination.set_transcript_correction(&full.id,original[0].segment.id,Some("must remain read only")).is_err());
        let effective = destination.transcript_version(&full.id,&full_id).unwrap().unwrap();
        let current = destination.segments(&full.id).unwrap();
        assert_eq!(effective.iter().map(|row|&row.segment).collect::<Vec<_>>(),current.iter().collect::<Vec<_>>());
        assert!(effective.iter().all(|row|row.provenance.is_some()));
        assert_eq!(current.last().unwrap().clean_text.as_deref(),Some("user corrected words"));
        assert!(current.last().unwrap().clean_text_manual);
        assert_ne!(current[0].id,source.segments(&full.id).unwrap()[0].id);
        let partial_listing = destination.transcript_versions(&partial.id).unwrap();
        assert_eq!(partial_listing.effective_id.as_deref(),Some("original"));
        assert!(partial_listing.versions.iter().any(|row|row.source == TranscriptVersionSource::Combined && row.partial));
        drop(destination);
        let destination = Store::open(&db).unwrap();
        assert_eq!(destination.transcript_versions(&full.id).unwrap(),listing);
        assert_eq!(import_bundle(&destination,&bundle).unwrap().skipped_existing,2);
    }

    #[test]
    fn malicious_version_history_is_rejected_before_importing_any_meeting() {
        let root = tempfile::tempdir().unwrap();
        let source = Store::open(&root.path().join("source.sqlite")).unwrap();
        let (meeting,_,_) = history_meeting(&source,true);
        let original = export_bundle(&source,&[meeting.id.clone()],&root.path().join("valid.bazza"),false).unwrap();
        for mode in 0..7 {
            let mut manifest = serde_json::to_value(&original).unwrap();
            let archive = &mut manifest["meetings"][0]["transcript_versions"];
            match mode {
                0 => archive["schema"] = serde_json::json!(2),
                1 => archive["effective_id"] = serde_json::json!(archive["alternatives"].as_array().unwrap().iter().find(|row|row["metadata"]["partial"] == true).unwrap()["metadata"]["id"]),
                2 => archive["alternatives"][0]["segments"][0]["provenance"]["participant_id"] = serde_json::json!(""),
                3 => archive["credentials"] = serde_json::json!({"token":"must not be accepted"}),
                4 => archive["alternatives"][0]["metadata"]["id"] = serde_json::json!("../escape"),
                5 => archive["original"]["segments"][0]["meeting_id"] = serde_json::json!("foreign meeting"),
                _ => manifest["meetings"][0]["segments"][0]["id"] = serde_json::json!(0),
            }
            let path = root.path().join(format!("invalid-{mode}.bazza"));
            let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
            zip.start_file("manifest.json",SimpleFileOptions::default()).unwrap();
            zip.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
            zip.start_file(format!("media/{}/mix.wav",meeting.id),SimpleFileOptions::default()).unwrap();
            zip.write_all(b"RIFF fixture").unwrap();
            zip.finish().unwrap();
            let dst_root = tempfile::tempdir().unwrap();
            let destination = Store::open(&dst_root.path().join("bazza.db")).unwrap();
            assert!(import_bundle(&destination,&path).is_err(),"mode {mode}");
            assert!(destination.get_meeting(&meeting.id).unwrap().is_none());
        }
    }

    #[test]
    fn bundle_roundtrip_merges_without_duplicates() {
        let src_dir = tempfile::tempdir().unwrap();
        let src = Store::open(&src_dir.path().join("bazza.db")).unwrap();
        let mut m = Meeting::new_recording("Budget", Some("zoom"));
        m.status = MeetingStatus::Ready;
        m.duration_ms = 4000;
        m.has_video = true;
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
        src.add_voice_sample("11111111-1111-4111-8111-111111111111",&bazza_store::NewVoiceSample {
            speaker_id:&profile.id,embedding:&[1.0,0.0],model_id:bazza_store::CURRENT_VOICE_MODEL,
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

        let out = src_dir.path().join("export.bazza");
        let manifest = export_bundle(&src, &[m.id.clone()], &out, true).unwrap();
        assert_eq!(manifest.app, "bazza");
        assert_eq!(manifest.meetings[0].media, vec!["mix.wav","screen.mp4"]);
        let encoded = serde_json::to_string(&manifest).unwrap();
        assert!(!encoded.contains("private-reference-voice") && !encoded.contains("synthetic-secret-not-for-export") && !encoded.contains("embedding"));
        assert!(!encoded.contains("synthetic-key-inside-extra-field") && !encoded.contains("synthetic-caption-secret"));
        assert_eq!(manifest.meetings[0].transcript.as_ref(),Some(&provenance));
        let zip = zip::ZipArchive::new(File::open(&out).unwrap()).unwrap();
        assert_eq!(zip.len(),3,"only manifest and selected media are exported");
        assert!(zip.file_names().all(|name| name == "manifest.json" || name.ends_with("/mix.wav") || name.ends_with("/screen.mp4")));

        let dst_dir = tempfile::tempdir().unwrap();
        let dst = Store::open(&dst_dir.path().join("bazza.db")).unwrap();
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

    #[test]
    fn manual_corrections_roundtrip_and_v1_missing_flag_defaults_false() {
        let root = tempfile::tempdir().unwrap();
        let source = Store::open(&root.path().join("source/bazza.db")).unwrap();
        let mut ids = Vec::new();
        for cleaned in ["manual corrected comet", ""] {
            let mut meeting = Meeting::new_recording("manual roundtrip", None);
            meeting.status = MeetingStatus::Ready; meeting.duration_ms = 1000;
            source.create_meeting(&meeting).unwrap();
            source.replace_segments(&meeting.id, &[NewSegment {start_ms:0,end_ms:1000,speaker:Some("Ann".into()),text:"original provider words".into()}]).unwrap();
            let segment = source.segments(&meeting.id).unwrap().remove(0);
            source.set_transcript_correction(&meeting.id, segment.id, Some(cleaned)).unwrap();
            source.set_setting(&format!("stt:{}",meeting.id), &serde_json::json!({"provider":"soniox","model":"fixture","language":"en","status":"complete","error":null,
                "captions":[{"start_ms":0,"end_ms":1000,"text":"original provider words","is_final":true}]})).unwrap();
            std::fs::write(source.recordings_dir(&meeting.id).join("mix.wav"), b"RIFFfake").unwrap();
            ids.push(meeting.id);
        }
        let bundle = root.path().join("manual.bazza");
        let manifest = export_bundle(&source, &ids, &bundle, false).unwrap();
        assert_eq!(manifest.version, 1);
        assert!(manifest.meetings.iter().all(|meeting| meeting.transcript_versions.is_none()),
            "provider metadata alone must not invent a duplicate version archive");
        assert!(manifest.meetings.iter().all(|meeting| meeting.segments[0].clean_text_manual));
        let destination_path = root.path().join("destination/bazza.db");
        let destination = Store::open(&destination_path).unwrap();
        assert_eq!(import_bundle(&destination, &bundle).unwrap().imported, 2);
        drop(destination);
        let destination = Store::open(&destination_path).unwrap();
        for (index, id) in ids.iter().enumerate() {
            let segment = destination.segments(id).unwrap().remove(0);
            assert_eq!(segment.text, "original provider words"); assert!(segment.clean_text_manual);
            assert_eq!((segment.start_ms,segment.end_ms,segment.speaker.as_deref()), (0,1000,Some("Ann")));
            assert_eq!(segment.clean_text.as_deref(), Some(if index == 0 { "manual corrected comet" } else { "" }));
            destination.set_clean_text(id, &[(segment.id,"AI overwrite".into())]).unwrap();
            assert_eq!(destination.segments(id).unwrap()[0], segment);
            assert_eq!(destination.get_setting::<serde_json::Value>(&format!("stt:{id}")).unwrap(), source.get_setting::<serde_json::Value>(&format!("stt:{id}")).unwrap());
        }
        let mut additive = serde_json::to_value(&manifest).unwrap();
        for meeting in additive["meetings"].as_array_mut().unwrap() {
            for segment in meeting["segments"].as_array_mut().unwrap() { segment.as_object_mut().unwrap().remove("clean_text_manual"); }
        }
        let without_flag: BundleManifest = serde_json::from_value(additive.clone()).unwrap();
        assert!(without_flag.meetings.iter().all(|meeting| !meeting.segments[0].clean_text_manual));
        let missing_flag = root.path().join("missing-flag.bazza");
        write_json_fixture(&missing_flag, &additive, &[
            (&format!("media/{}/mix.wav",ids[0]),b"RIFFfake"), (&format!("media/{}/mix.wav",ids[1]),b"RIFFfake"),
        ]);
        assert!(read_manifest(&missing_flag).unwrap().meetings.iter().all(|meeting| !meeting.segments[0].clean_text_manual));
    }

    #[test]
    fn invalid_manual_bundle_overlay_is_rejected_before_any_import() {
        let root = tempfile::tempdir().unwrap();
        let destination = Store::open(&root.path().join("destination/bazza.db")).unwrap();
        for (index, text) in [None,Some("invalid\0text".into()),Some("x".repeat(bazza_store::MAX_TRANSCRIPT_CORRECTION_BYTES + 1))].into_iter().enumerate() {
            let mut manifest = fixture(); let id = manifest.meetings[0].meeting.id.clone();
            manifest.meetings[0].segments.push(Segment {id:0,meeting_id:id.clone(),start_ms:0,end_ms:1000,speaker:None,text:"original".into(),clean_text:text,clean_text_manual:true});
            let bundle = root.path().join(format!("manual-invalid-{index}.bazza"));
            write_fixture(&bundle, &manifest, &[(&format!("media/{id}/mix.wav"),b"RIFFfake")]);
            assert!(import_bundle(&destination, &bundle).is_err());
            assert!(destination.list_meetings().unwrap().is_empty());
            assert!(!destination.root().join("recordings").exists());
        }
    }

    fn fixture() -> BundleManifest {
        let mut meeting = Meeting::new_recording("fixture",None);
        meeting.status = MeetingStatus::Ready;
        BundleManifest { version:1,exported_at:chrono::Utc::now(),app:"bazza".into(),meetings:vec![BundleMeeting {
            meeting,segments:vec![],speakers:vec![],summary:None,highlights:vec![],tags:vec![],media:vec!["mix.wav".into()],transcript:None,transcript_versions:None,
        }] }
    }

    fn write_fixture(path: &Path, manifest: &BundleManifest, entries: &[(&str,&[u8])]) {
        write_json_fixture(path, &serde_json::to_value(manifest).unwrap(), entries);
    }

    fn write_json_fixture(path: &Path, manifest: &serde_json::Value, entries: &[(&str,&[u8])]) {
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
    fn foreign_product_manifest_and_non_bazza_extension_are_rejected_before_import() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
        let mut manifest = fixture();
        manifest.app = "foreign-product".into();
        let id = manifest.meetings[0].meeting.id.clone();
        let foreign = root.path().join("foreign.bazza");
        write_fixture(&foreign, &manifest, &[(&format!("media/{id}/mix.wav"), b"audio")]);
        assert!(read_manifest(&foreign).unwrap_err().to_string().contains("Bazza"));
        assert!(import_bundle(&store, &foreign).is_err());
        manifest.app = "bazza".into();
        let wrong_extension = root.path().join("foreign.package");
        write_fixture(&wrong_extension, &manifest, &[(&format!("media/{id}/mix.wav"), b"audio")]);
        assert!(read_manifest(&wrong_extension).unwrap_err().to_string().contains(".bazza"));
        assert!(import_bundle(&store, &wrong_extension).is_err());
        let wrong_output = root.path().join("export.package");
        assert!(export_bundle(&store, &[], &wrong_output, false).is_err());
        assert!(!wrong_output.exists());
        assert!(store.list_meetings().unwrap().is_empty());
        assert!(!store.root().join("recordings").exists());
    }

    #[test]
    fn malformed_bundles_never_write_media_or_metadata() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("library/bazza.db")).unwrap();
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
                6 => manifest.meetings[0].segments.push(Segment {id:0,meeting_id:"other".into(),start_ms:0,end_ms:1,speaker:None,text:"bad".into(),clean_text:None,clean_text_manual:false}),
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
            let path = root.path().join(format!("{mode}.bazza"));
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
        let store = Store::open(&root.path().join("library/bazza.db")).unwrap();
        let mut manifest = fixture();
        let first = manifest.meetings[0].meeting.id.clone();
        let second_meeting = fixture().meetings.remove(0);
        let second = second_meeting.meeting.id.clone();
        manifest.meetings.push(second_meeting);
        std::fs::write(store.recordings_dir(&first).join("mic.wav"),b"original").unwrap();
        let path = root.path().join("two.bazza");
        write_fixture(&path,&manifest,&[
            (&format!("media/{first}/mix.wav"),b"one"),
            (&format!("media/{second}/mix.wav"),b"two"),
        ]);
        let report = import_bundle(&store,&path).unwrap();
        assert_eq!((report.imported,report.errors.len()),(1,1));
        assert!(store.get_meeting(&first).unwrap().is_none());
        assert!(store.get_meeting(&second).unwrap().is_some());
        assert_eq!(std::fs::read(store.recordings_dir(&first).join("mic.wav")).unwrap(),b"original");
        std::fs::write(store.recordings_dir(&first).join(".bazza-import-pending"),&first).unwrap();
        let report = import_bundle(&store,&path).unwrap();
        assert_eq!((report.imported,report.skipped_existing),(1,1));
        assert!(report.errors.is_empty());
        assert_eq!(std::fs::read(store.recordings_dir(&first).join("mix.wav")).unwrap(),b"one");
        assert!(!std::fs::read_dir(store.root().join("recordings")).unwrap().any(|e|e.unwrap().file_name().to_string_lossy().starts_with(".bazza-import-")));
    }

    #[test]
    fn export_without_video_reports_audio_only() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
        let mut meeting = Meeting::new_recording("audio",None);
        meeting.status = MeetingStatus::Ready;
        meeting.has_video = true;
        store.create_meeting(&meeting).unwrap();
        std::fs::write(store.recordings_dir(&meeting.id).join("mix.wav"),b"audio").unwrap();
        std::fs::write(store.recordings_dir(&meeting.id).join("screen.mp4"),b"video").unwrap();
        let manifest = export_bundle(&store,&[meeting.id],&root.path().join("audio.bazza"),false).unwrap();
        assert!(!manifest.meetings[0].meeting.has_video);
        assert_eq!(manifest.meetings[0].media,vec!["mix.wav"]);
    }

    #[test]
    fn retained_unusable_video_is_not_exported_or_imported_as_playable_video() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("source/bazza.db")).unwrap();
        let mut meeting = Meeting::new_recording("recovered audio", None);
        meeting.status = MeetingStatus::Ready;
        meeting.has_video = false;
        store.create_meeting(&meeting).unwrap();
        let media = store.recordings_dir(&meeting.id);
        std::fs::write(media.join("mix.wav"), b"saved audio").unwrap();
        std::fs::write(media.join("screen.mp4"), b"retained unusable video").unwrap();

        let package = root.path().join("audio.bazza");
        let manifest = export_bundle(&store, &[meeting.id.clone()], &package, true).unwrap();
        assert!(!manifest.meetings[0].meeting.has_video);
        assert_eq!(manifest.meetings[0].media, vec!["mix.wav"]);
        let archive = zip::ZipArchive::new(File::open(&package).unwrap()).unwrap();
        assert_eq!(archive.len(), 2, "only the manifest and playable audio are exported");
        assert!(!archive.file_names().any(|name| name.ends_with("/screen.mp4")));

        let destination = Store::open(&root.path().join("destination/bazza.db")).unwrap();
        let report = import_bundle(&destination, &package).unwrap();
        assert_eq!(report.imported, 1);
        assert!(report.errors.is_empty());
        assert!(!destination.get_meeting(&meeting.id).unwrap().unwrap().has_video);
        let imported_media = destination.recordings_dir(&meeting.id);
        assert_eq!(std::fs::read(imported_media.join("mix.wav")).unwrap(), b"saved audio");
        assert!(!imported_media.join("screen.mp4").exists());
        assert_eq!(std::fs::read(media.join("screen.mp4")).unwrap(), b"retained unusable video");
    }

    #[test]
    fn active_export_is_rejected_before_creating_output() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(&root.path().join("bazza.db")).unwrap();
        let mut meeting = Meeting::new_recording("active",None);
        store.create_meeting(&meeting).unwrap();
        let out = root.path().join("active.bazza");
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
        let store = Store::open(&root.path().join("library/bazza.db")).unwrap();
        let manifest = fixture();
        let id = &manifest.meetings[0].meeting.id;
        let path = root.path().join("corrupt.bazza");
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
        let path = root.path().join("duplicate.bazza");
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
        let path = root.path().join("large.bazza");
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        writer.start_file("manifest.json",SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
        writer.write_all(&vec![b' '; MAX_MANIFEST_BYTES as usize + 1]).unwrap();
        writer.finish().unwrap();
        assert!(read_manifest(&path).unwrap_err().to_string().contains("16 MiB"));
    }

    #[test]
    fn excessive_declared_entries_are_rejected_before_zip_index_allocation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("count.bazza");
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
        let path = root.path().join("zip64.bazza");
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
