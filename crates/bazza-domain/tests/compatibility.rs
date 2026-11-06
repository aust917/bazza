use bazza_domain::{
    validate_transcript_correction, ChunkHit, ManualCorrectionError, Meeting, MeetingFilter,
    MeetingSpeaker, MeetingStatus, Segment, Speaker, MAX_TRANSCRIPT_CORRECTION_BYTES,
};
use serde_json::json;

#[test]
fn recording_constructor_and_legacy_meeting_payload_keep_defaults_and_dates() {
    let recording = Meeting::new_recording(" Exact title ", Some("zoom"));
    assert!(uuid::Uuid::parse_str(&recording.id).is_ok());
    assert_eq!(recording.title, " Exact title ");
    assert_eq!(recording.app.as_deref(), Some("zoom"));
    assert_eq!(recording.status, MeetingStatus::Recording);
    assert_eq!(recording.started_at, recording.created_at);
    assert!(recording.ended_at.is_none());
    assert_eq!(recording.duration_ms, 0);
    assert!(!recording.has_video);
    assert!(recording.title_auto);
    assert!(recording.participants.is_empty());
    assert!(recording.folder_id.is_none());
    assert!(recording.error.is_none());
    assert!(recording.calendar_uid.is_none());

    let legacy: Meeting = serde_json::from_value(json!({
        "id":"legacy-import_42", "title":"Старая встреча", "app":null,
        "started_at":"2026-11-02T03:04:05.123456789+03:00", "ended_at":null,
        "duration_ms":42, "has_video":false, "status":"ready", "folder_id":null,
        "created_at":"2026-11-02T00:04:06Z"
    }))
    .unwrap();
    let encoded = serde_json::to_value(&legacy).unwrap();
    assert_eq!(encoded["started_at"], "2026-11-02T00:04:05.123456789Z");
    assert_eq!(encoded["created_at"], "2026-11-02T00:04:06Z");
    assert_eq!(encoded["error"], json!(null));
    assert_eq!(encoded["title_auto"], true);
    assert_eq!(encoded["participants"], json!([]));
    assert_eq!(encoded["calendar_uid"], json!(null));
    assert_eq!(serde_json::from_value::<Meeting>(encoded).unwrap(), legacy);
    assert!(serde_json::from_str::<MeetingStatus>("\"unknown-status\"").is_err());
}

#[test]
fn old_segments_and_manual_empty_text_keep_raw_content_and_byte_validation() {
    let mut segment: Segment = serde_json::from_value(json!({
        "id":7, "meeting_id":"legacy-import_42", "start_ms":10, "end_ms":20,
        "speaker":"Анна", "text":" Original raw текст ", "clean_text":""
    }))
    .unwrap();
    assert!(!segment.clean_text_manual);
    segment.clean_text_manual = true;
    segment.validate_manual_correction().unwrap();
    assert_eq!(segment.clean_text.as_deref().unwrap_or(&segment.text), "");
    assert_eq!(segment.text, " Original raw текст ");
    assert_eq!(
        serde_json::from_str::<Segment>(&serde_json::to_string(&segment).unwrap()).unwrap(),
        segment
    );
    segment.clean_text = None;
    assert_eq!(
        segment.validate_manual_correction(),
        Err(ManualCorrectionError::Invalid)
    );

    let boundary = "я".repeat(MAX_TRANSCRIPT_CORRECTION_BYTES / 2);
    validate_transcript_correction(Some(&boundary)).unwrap();
    assert_eq!(
        validate_transcript_correction(Some(&(boundary + "я"))),
        Err(ManualCorrectionError::TooLarge)
    );
    assert_eq!(
        validate_transcript_correction(Some("raw\0hidden")),
        Err(ManualCorrectionError::Invalid)
    );
    assert_eq!(
        ManualCorrectionError::Invalid.to_string(),
        "invalid transcript correction"
    );
    assert_eq!(
        ManualCorrectionError::TooLarge.to_string(),
        "transcript correction exceeds 65536 UTF-8 bytes"
    );
}

#[test]
fn folder_filter_retains_legacy_and_picker_wire_forms() {
    for (wire, expected) in [
        (json!({}), None),
        (json!({"folder":null}), None),
        (json!({"folder":[null]}), Some(None)),
        (
            json!({"folder":"legacy-folder"}),
            Some(Some("legacy-folder".to_string())),
        ),
        (
            json!({"folder":["legacy-folder"]}),
            Some(Some("legacy-folder".to_string())),
        ),
    ] {
        let filter: MeetingFilter = serde_json::from_value(wire).unwrap();
        assert_eq!(filter.folder, expected);
        assert!(filter.tag_id.is_none());
    }
    for wire in [
        json!({"folder":[]}),
        json!({"folder":[null,null]}),
        json!({"folder":4}),
    ] {
        assert!(serde_json::from_value::<MeetingFilter>(wire).is_err());
    }
}

#[test]
fn voice_vectors_scores_and_retrieval_ranks_keep_their_float_precision() {
    let speaker = Speaker {
        id: "voice".into(),
        name: "Анна".into(),
        embedding: vec![-0.0, 0.1, -0.625, f32::MIN_POSITIVE, f32::MAX],
    };
    let decoded: Speaker = serde_json::from_str(&serde_json::to_string(&speaker).unwrap()).unwrap();
    assert_eq!(
        decoded
            .embedding
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>(),
        speaker
            .embedding
            .iter()
            .map(|value| value.to_bits())
            .collect::<Vec<_>>()
    );
    let row = MeetingSpeaker {
        meeting_id: "meeting".into(),
        label: "Speaker 1".into(),
        speaker_id: None,
        suggested_id: Some("voice".into()),
        suggested_name: Some("Анна".into()),
        suggested_score: Some(0.620_000_06),
    };
    let decoded: MeetingSpeaker =
        serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
    assert_eq!(
        decoded.suggested_score.unwrap().to_bits(),
        row.suggested_score.unwrap().to_bits()
    );
    for rank in [-0.0_f64, -1.5, 0.123_456_789_012_345_66, 1.0e30] {
        let hit = ChunkHit {
            meeting_id: "meeting".into(),
            meeting_title: "Title".into(),
            started_at: chrono::DateTime::from_timestamp(1_783_123_456, 987_654_321).unwrap(),
            start_ms: 10,
            end_ms: 20,
            text: "Raw текст".into(),
            rank,
        };
        let decoded: ChunkHit =
            serde_json::from_str(&serde_json::to_string(&hit).unwrap()).unwrap();
        assert_eq!(decoded.rank.to_bits(), rank.to_bits());
        assert_eq!(decoded.started_at, hit.started_at);
    }
}
