//! Typed domain entry point, currently reachable only on explicit synthetic
//! repositories. No capture, filesystem path input, model or network startup.
use crate::Repository;
pub use bazza_store::service_data::*;

pub struct DataRepository<'a> {
    store: &'a bazza_store::Store,
}

impl Repository {
    pub fn data(&self) -> DataRepository<'_> {
        DataRepository { store: &self.store }
    }
}
impl DataRepository<'_> {
    pub fn create_meeting(
        &self,
        ctx: &MutationContext,
        input: &NewTextMeeting,
    ) -> DataResult<MutationOutcome> {
        self.store.service_create_text_meeting(ctx, input)
    }
    pub fn meeting(&self, id: &str) -> DataResult<Option<ServiceMeeting>> {
        self.store.service_meeting(id)
    }
    pub fn update_meeting(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        patch: &MeetingUpdate,
    ) -> DataResult<MutationOutcome> {
        self.store.service_update_meeting(ctx, id, expected, patch)
    }
    pub fn create_note(
        &self,
        ctx: &MutationContext,
        input: &NewNote,
    ) -> DataResult<MutationOutcome> {
        self.store.service_create_note(ctx, input)
    }
    pub fn note(&self, id: &str) -> DataResult<Option<UserNote>> {
        self.store.service_note(id)
    }
    pub fn notes(
        &self,
        meeting: Option<&str>,
        standalone: bool,
        after: Option<&str>,
        limit: usize,
    ) -> DataResult<Vec<NoteListEntry>> {
        self.store.service_notes(meeting, standalone, after, limit)
    }
    pub fn update_note(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
        patch: &NoteUpdate,
    ) -> DataResult<MutationOutcome> {
        self.store.service_update_note(ctx, id, expected, patch)
    }
    pub fn delete_note(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        self.store.service_delete_note(ctx, id, expected)
    }
    pub fn transcript_state(&self, id: &str) -> DataResult<TranscriptState> {
        self.store.service_transcript_state(id)
    }
    pub fn transcript(&self, id: &str, input: &TranscriptRead) -> DataResult<TranscriptSourcePage> {
        self.store.service_read_transcript(id, input)
    }
    pub fn import_transcript(
        &self,
        ctx: &MutationContext,
        id: &str,
        input: &TranscriptWrite,
    ) -> DataResult<MutationOutcome> {
        self.store.service_import_transcript(ctx, id, input)
    }
    pub fn replace_transcript(
        &self,
        ctx: &MutationContext,
        id: &str,
        input: &TranscriptWrite,
    ) -> DataResult<MutationOutcome> {
        self.store.service_replace_transcript(ctx, id, input)
    }
    pub fn correct_transcript(
        &self,
        ctx: &MutationContext,
        id: &str,
        input: &TranscriptCorrections,
    ) -> DataResult<MutationOutcome> {
        self.store
            .service_set_transcript_corrections(ctx, id, input)
    }
    pub fn delete_transcript(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        self.store.service_delete_transcript(ctx, id, expected)
    }
    pub fn summary(&self, id: &str) -> DataResult<SummarySnapshot> {
        self.store.service_read_summary(id)
    }
    pub fn write_summary(
        &self,
        ctx: &MutationContext,
        id: &str,
        input: &SummaryWrite,
    ) -> DataResult<MutationOutcome> {
        self.store.service_write_summary(ctx, id, input)
    }
    pub fn delete_summary(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        self.store.service_delete_summary(ctx, id, expected)
    }
    pub fn search(&self, input: &SearchRequest) -> DataResult<SearchPage> {
        self.store.service_search(input)
    }
    pub fn changes(&self, after: i64, limit: usize) -> DataResult<Vec<Change>> {
        self.store.service_changes(after, limit)
    }
    pub fn delete_meeting(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        self.store.service_delete_meeting(ctx, id, expected)
    }
    pub fn job(&self, id: &str) -> DataResult<DurableJob> {
        self.store.service_job(id)
    }
    /// Trusted service cleanup entry, not an external worker/MCP operation.
    pub fn run_purge(&self, job_id: &str) -> DataResult<bool> {
        self.store.service_run_purge(job_id)
    }
    pub fn cancel_job(
        &self,
        ctx: &MutationContext,
        id: &str,
        expected: &str,
    ) -> DataResult<MutationOutcome> {
        self.store.service_cancel_job(ctx, id, expected)
    }
    pub fn calendar_cache(&self, id: &str) -> DataResult<CalendarCache> {
        self.store.service_calendar_cache(id)
    }
}
