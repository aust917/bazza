use super::*;
use serde_json::json;

#[test]
fn preserves_source_order_and_explicit_null_and_keeps_applied_distinct_from_skip() {
    let input = json!({
        "kind": "applied",
        "assignments": [
            {"segment_id": 7, "cluster": 80},
            {"segment_id": 2, "cluster": null},
            {"segment_id": 5, "cluster": 80}
        ]
    });
    let applied = SpeakerResultBody::parse(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(applied.outcome(), SpeakerResultOutcome::Applied);
    assert_eq!(serde_json::to_value(applied).unwrap(), input);

    let applied_without_labels =
        br#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":null}]}"#;
    assert_eq!(
        SpeakerResultBody::parse(applied_without_labels)
            .unwrap()
            .outcome(),
        SpeakerResultOutcome::Applied,
    );
    assert_eq!(
        SpeakerResultBody::parse(br#"{"kind":"skipped","reason":"no_turns"}"#)
            .unwrap()
            .outcome(),
        SpeakerResultOutcome::Skipped {
            reason: SpeakerSkipReason::NoTurns
        },
    );
}

#[test]
fn rejects_out_of_shape_results_and_repeated_or_unsafe_segment_ids() {
    for input in [
        r#"{"kind":"applied","assignments":[]}"#,
        r#"["applied",[{"segment_id":7,"cluster":null}]]"#,
        r#"{"kind":"applied","assignments":[[7,null]]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":0,"cluster":null}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":9007199254740992,"cluster":null}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":0},{"segment_id":7,"cluster":null}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":-1}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":4294967296}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":null,"name":"caller"}]}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":null,"speaker_id":"profile"}]}"#,
        r#"{"kind":"applied","assignments":[],"embeddings":[]}"#,
        r#"{"kind":"skipped","reason":"sidecar_failed","error":"caller"}"#,
        r#"{"kind":"skipped","reason":"disabled","assignments":null}"#,
        r#"{"kind":"applied","assignments":[{"segment_id":7,"cluster":null}],"reason":null}"#,
        r#"{"kind":"skipped","reason":"other"}"#,
    ] {
        assert!(
            SpeakerResultBody::parse(input.as_bytes()).is_err(),
            "{input}"
        );
    }
}

#[test]
fn bounds_encoded_bytes_and_decoded_assignments_independently() {
    let mut bytes = br#"{"kind":"skipped","reason":"disabled"}"#.to_vec();
    bytes.resize(MAX_SPEAKER_RESULT_BODY_BYTES as usize, b' ');
    assert!(SpeakerResultBody::parse(&bytes).is_ok());
    bytes.push(b' ');
    assert!(SpeakerResultBody::parse(&bytes).is_err());

    let mut assignments: Vec<_> = (1..=MAX_SPEAKER_ASSIGNMENTS as i64)
        .map(|segment_id| SpeakerAssignment {
            segment_id,
            cluster: None,
        })
        .collect();
    let bytes = serde_json::to_vec(&SpeakerResultBody::Applied {
        assignments: assignments.clone(),
    })
    .unwrap();
    assert!(SpeakerResultBody::parse(&bytes).is_ok());
    assignments.push(SpeakerAssignment {
        segment_id: MAX_SEGMENT_ID,
        cluster: None,
    });
    let bytes = serde_json::to_vec(&SpeakerResultBody::Applied { assignments }).unwrap();
    assert!(serde_json::from_slice::<SpeakerResultBody>(&bytes).is_err());
}
