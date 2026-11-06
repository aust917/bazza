use super::{WorkError, WorkResult};
use bazza_service_client::{
    api::{
        content::DesktopTranscriptFragment,
        postprocess_source::{MAX_LOCAL_ASR_CAPTIONS, MAX_POST_PROCESS_SOURCE_BODY_BYTES},
    },
    ClientError,
};
use std::collections::HashSet;

pub(super) struct TranscriptRow {
    pub segment_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
    pub clean_text_manual: bool,
}

#[derive(Default)]
pub(super) struct Transcript {
    rows: Vec<TranscriptRow>,
    ids: HashSet<i64>,
    bytes: u64,
    unfinished: Option<(TranscriptRow, u64)>,
}

impl Transcript {
    pub(super) fn push(&mut self, fragment: DesktopTranscriptFragment) -> WorkResult<()> {
        if fragment.segment_index != self.rows.len() as u64 {
            return Err(ClientError::Protocol.into());
        }
        if self.unfinished.is_none() {
            if self.rows.len() >= MAX_LOCAL_ASR_CAPTIONS
                || self.bytes + fragment.total_bytes > MAX_POST_PROCESS_SOURCE_BODY_BYTES
            {
                return Err(WorkError::Deferred(
                    "transcript exceeds the native worker budget",
                ));
            }
            if fragment.byte_offset != 0 || !self.ids.insert(fragment.segment_id) {
                return Err(ClientError::Protocol.into());
            }
            self.bytes += fragment.total_bytes;
            self.unfinished = Some((
                TranscriptRow {
                    segment_id: fragment.segment_id,
                    start_ms: fragment.start_ms,
                    end_ms: fragment.end_ms,
                    speaker: fragment.speaker.clone(),
                    text: String::new(),
                    clean_text_manual: fragment.clean_text_manual,
                },
                fragment.total_bytes,
            ));
        }
        let (row, expected) = self.unfinished.as_mut().ok_or(ClientError::Protocol)?;
        if row.segment_id != fragment.segment_id
            || row.start_ms != fragment.start_ms
            || row.end_ms != fragment.end_ms
            || row.speaker != fragment.speaker
            || row.clean_text_manual != fragment.clean_text_manual
            || row.text.len() as u64 != fragment.byte_offset
            || *expected != fragment.total_bytes
            || fragment.byte_offset + fragment.text.len() as u64 > *expected
        {
            return Err(ClientError::Protocol.into());
        }
        row.text.push_str(&fragment.text);
        if row.text.len() as u64 == *expected {
            self.rows
                .push(self.unfinished.take().ok_or(ClientError::Protocol)?.0);
        }
        Ok(())
    }

    pub(super) fn finish(self) -> WorkResult<Vec<TranscriptRow>> {
        if self.unfinished.is_some() {
            return Err(ClientError::Protocol.into());
        }
        Ok(self.rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bazza_service_client::api::content::DesktopEffectiveOrigin;

    fn fragment(offset: u64, text: &str) -> DesktopTranscriptFragment {
        DesktopTranscriptFragment {
            segment_index: 0,
            segment_id: 11,
            start_ms: 0,
            end_ms: 50,
            speaker: None,
            text: text.into(),
            byte_offset: offset,
            total_bytes: "ёж".len() as u64,
            has_clean_text: false,
            clean_text_manual: false,
            effective_origin: DesktopEffectiveOrigin::Raw,
        }
    }

    #[test]
    fn fragments_join_on_exact_byte_offsets() {
        let mut transcript = Transcript::default();
        transcript.push(fragment(0, "ё")).unwrap();
        assert!(transcript.push(fragment(1, "ж")).is_err());
        transcript.push(fragment(2, "ж")).unwrap();
        let rows = transcript.finish().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "ёж");
    }

    #[test]
    fn incomplete_final_segment_cannot_be_used() {
        let mut transcript = Transcript::default();
        transcript.push(fragment(0, "ё")).unwrap();
        assert!(transcript.finish().is_err());
    }
}
