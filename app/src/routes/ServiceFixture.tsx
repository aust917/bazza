import { useEffect, useRef, useState } from "react";
import {
  fixtureAvailable, sameRepository, serviceFixture,
  type ChangePage, type DeleteMeetingRequest, type DesktopMeeting, type FixtureCapture, type FixtureError, type MeetingFilter, type MeetingPage,
  type Preferences, type RepositoryIdentity, type Representation, type SummaryField,
  type SummaryPage, type TranscriptPage,
} from "@/lib/service-fixture-ipc";

const PAGE_LIMIT = 20;
const PAGE_BYTES = 16_384;
const emptyFilter: MeetingFilter = { folder: { kind: "all" }, tag_id: null, content_kind: null };
type Channel = "preferences" | "meetings" | "meeting" | "rename" | "remove" | "transcript" | "summary" | "changes";
type Ticket = { generation: number; channel: Channel; token: number; identity: RepositoryIdentity };
const channels: Channel[] = ["preferences", "meetings", "meeting", "rename", "remove", "transcript", "summary", "changes"];

function serviceError(error: unknown): FixtureError {
  if (typeof error === "object" && error !== null && "code" in error && "message" in error
    && typeof error.code === "string" && /^[A-Z_]+$/.test(error.code) && typeof error.message === "string") {
    return { code: error.code, message: error.message.slice(0, 512), retryable: "retryable" in error && error.retryable === true };
  }
  return { code: "SERVICE_UNAVAILABLE", message: "The fixture service is unavailable.", retryable: false };
}

function CaptureControls({ canStart, onChanged }: { canStart: boolean; onChanged: () => void }) {
  const [capture, setCapture] = useState<FixtureCapture | null>(null);
  const [captureScreen, setCaptureScreen] = useState(false);
  const [configuredAudio, setConfiguredAudio] = useState(false);
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState<FixtureError | null>(null);
  const inFlight = useRef(true);
  const mounted = useRef(false);

  useEffect(() => {
    let current = true;
    mounted.current = true;
    void serviceFixture.captureStatus().then((view) => {
      if (current) setCapture(view);
    }).catch((cause) => {
      if (current) setError(serviceError(cause));
    }).finally(() => {
      if (current) {
        inFlight.current = false;
        setBusy(false);
      }
    });
    return () => {
      current = false;
      mounted.current = false;
    };
  }, []);

  async function request(action: "captureStatus" | "captureStart" | "capturePause" | "captureResume" | "captureStop") {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const view = action === "captureStart"
        ? await serviceFixture.captureStart(captureScreen, configuredAudio)
        : await serviceFixture[action]();
      if (mounted.current) setCapture(view);
    } catch (cause) {
      if (mounted.current) {
        setCapture(null);
        setError(serviceError(cause));
      }
      if (action === "capturePause" || action === "captureResume") {
        // The audio transition can succeed before a video timing write fails.
        const view = await serviceFixture.captureStatus().catch(() => null);
        if (mounted.current) setCapture(view);
      }
    } finally {
      inFlight.current = false;
      if (mounted.current) {
        if (action === "captureStart" || action === "captureStop") onChanged();
        setBusy(false);
      }
    }
  }

  const paused = capture?.state === "paused";
  const active = paused || capture?.state === "recording";
  const stopEnabled = active || capture?.state === "pending";
  return <section aria-label="Microphone capture" className="flex flex-col gap-2">
    <h2 className="font-semibold">Microphone capture</h2>
    <p className="text-sm text-muted">By default, capture uses only the default microphone. Stop the capture before closing the window.</p>
    <label>
      <input type="checkbox" checked={configuredAudio} disabled={busy || capture?.state !== "idle"}
        onChange={(event) => setConfiguredAudio(event.target.checked)} />
      {" "}Use saved audio devices
    </label>
    {configuredAudio && <p className="text-sm text-muted">System audio is included when the saved mode is set to all audio.</p>}
    <label>
      <input type="checkbox" checked={captureScreen} disabled={busy || capture?.state !== "idle"}
        onChange={(event) => setCaptureScreen(event.target.checked)} />
      {" "}Include primary screen
    </label>
    <div className="flex flex-wrap gap-2">
      <button type="button" disabled={busy || !canStart || capture?.state !== "idle"}
        onClick={() => void request("captureStart")}>Start microphone capture</button>
      <button type="button" disabled={busy || !active}
        onClick={() => void request(paused ? "captureResume" : "capturePause")}>{paused ? "Resume capture" : "Pause capture"}</button>
      <button type="button" disabled={busy || !stopEnabled}
        onClick={() => void request("captureStop")}>{capture?.state === "pending" ? "Retry finalization" : "Stop microphone capture"}</button>
      <button type="button" disabled={busy} onClick={() => void request("captureStatus")}>Refresh capture status</button>
    </div>
    <p className="break-all text-sm" aria-live="polite">
      {capture ? `Capture ${capture.state}${capture.meeting_id ? ` · meeting ${capture.meeting_id}` : ""}` : "Capture status unavailable"}
      {capture?.job_id && ` · job ${capture.job_id}`}
      {capture?.screen && ` · screen ${capture.screen}`}
    </p>
    {error && <p role="alert">{error.code}: {error.message}</p>}
  </section>;
}

// This window deliberately keeps only one page of each projection. It never uses
// the legacy settings, library, media, or browser-mock paths.
export function ServiceFixture() {
  const available = fixtureAvailable();
  const [identity, setIdentity] = useState<RepositoryIdentity | null>(null);
  const [preferences, setPreferences] = useState<Preferences | null>(null);
  const [draft, setDraft] = useState<Preferences | null>(null);
  const [filter, setFilter] = useState<MeetingFilter>(emptyFilter);
  const [meetings, setMeetings] = useState<MeetingPage | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [meeting, setMeeting] = useState<DesktopMeeting | null>(null);
  const [title, setTitle] = useState("");
  const [deleteRequest, setDeleteRequest] = useState<DeleteMeetingRequest | null>(null);
  const [representation, setRepresentation] = useState<Representation>("raw");
  const [transcript, setTranscript] = useState<TranscriptPage | null>(null);
  const [field, setField] = useState<SummaryField>("markdown");
  const [summary, setSummary] = useState<SummaryPage | null>(null);
  const [changes, setChanges] = useState<ChangePage | null>(null);
  const [error, setError] = useState<FixtureError | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [busy, setBusy] = useState<Partial<Record<Channel, boolean>>>({});
  const mounted = useRef(false);
  const generation = useRef(0);
  const tokens = useRef<Record<Channel, number>>({ preferences: 0, meetings: 0, meeting: 0, rename: 0, remove: 0, transcript: 0, summary: 0, changes: 0 });
  const identityRef = useRef<RepositoryIdentity | null>(null);
  const pinned = useRef<RepositoryIdentity | null>(null);

  function invalidate(...affected: Channel[]) {
    for (const channel of affected) tokens.current[channel] += 1;
    setBusy((value) => {
      const next = { ...value };
      for (const channel of affected) delete next[channel];
      return next;
    });
  }
  function clearSelection() {
    invalidate("meeting", "rename", "remove", "transcript", "summary");
    setDeleteRequest(null);
    setSelected(null); setMeeting(null); setTitle(""); setTranscript(null); setSummary(null);
  }
  function clearPages() {
    invalidate("meetings", "changes");
    setMeetings(null); setChanges(null); clearSelection();
  }
  function clearConnection() {
    generation.current += 1;
    invalidate(...channels);
    identityRef.current = null; setIdentity(null);
    setPreferences(null); setDraft(null); clearPages();
  }
  function current(ticket: Ticket): boolean {
    return mounted.current && ticket.generation === generation.current
      && tokens.current[ticket.channel] === ticket.token
      && sameRepository(identityRef.current, ticket.identity);
  }
  function fail(cause: unknown, channel?: Channel) {
    const failure = serviceError(cause);
    if (failure.code === "RESYNC_REQUIRED") clearConnection();
    else if (["CURSOR_STALE", "INVALID_CURSOR"].includes(failure.code)) clearPages();
    else if (failure.code === "CONFLICT") {
      if (channel === "preferences") { setPreferences(null); setDraft(null); }
      else { setMeetings(null); clearSelection(); }
    }
    setError(failure);
  }
  async function run<T extends { identity: RepositoryIdentity }>(channel: Channel, load: () => Promise<T>, apply: (value: T) => void) {
    const repository = identityRef.current;
    if (!repository) return;
    const ticket: Ticket = { generation: generation.current, channel, token: ++tokens.current[channel], identity: repository };
    setBusy((value) => ({ ...value, [channel]: true })); setError(null);
    try {
      const value = await load();
      if (!current(ticket)) return;
      if (!sameRepository(repository, value.identity)) {
        fail({ code: "RESYNC_REQUIRED", message: "The service identity changed. Reconnect to continue.", retryable: false });
        return;
      }
      apply(value);
    } catch (cause) {
      if (current(ticket)) fail(cause, channel);
    } finally {
      if (current(ticket)) setBusy((value) => ({ ...value, [channel]: false }));
    }
  }
  async function connect(reconnect: boolean) {
    clearConnection();
    const requestGeneration = generation.current;
    setConnecting(true); setError(null);
    try {
      const reconnected = reconnect ? await serviceFixture.reconnect() : null;
      if (!mounted.current || requestGeneration !== generation.current) return;
      const snapshot = await serviceFixture.bootstrap();
      if (!mounted.current || requestGeneration !== generation.current) return;
      if ((reconnected && !sameRepository(reconnected, snapshot.identity))
        || (pinned.current && (pinned.current.library_id !== snapshot.identity.library_id || pinned.current.cursor_scope !== snapshot.identity.cursor_scope))
        || snapshot.data.cursor_scope !== snapshot.identity.cursor_scope) {
        throw { code: "RESYNC_REQUIRED", message: "The fixture library identity changed.", retryable: false };
      }
      pinned.current = snapshot.identity;
      identityRef.current = snapshot.identity; setIdentity(snapshot.identity);
      setPreferences(snapshot.data.preferences); setDraft(snapshot.data.preferences);
    } catch (cause) {
      if (mounted.current && requestGeneration === generation.current) setError(serviceError(cause));
    } finally {
      if (mounted.current && requestGeneration === generation.current) setConnecting(false);
    }
  }
  useEffect(() => {
    mounted.current = true;
    if (available) void connect(false);
    return () => { mounted.current = false; generation.current += 1; };
    // One bootstrap per window mount. All later refreshes are explicit actions.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [available]);

  function changeFilter(next: MeetingFilter) {
    clearPages(); setFilter(next); setError(null);
  }
  function readMeetings(next: boolean) {
    const cursor = next ? meetings?.next ?? null : null;
    clearSelection();
    void run("meetings", () => serviceFixture.meetings({ filter, cursor, limit: PAGE_LIMIT, max_bytes: PAGE_BYTES }), setMeetings);
  }
  function selectMeeting(id: string) {
    clearSelection(); setSelected(id);
    void run("meeting", () => serviceFixture.meeting(id), (snapshot) => {
      setMeeting(snapshot.data); setTitle(snapshot.data?.title ?? "");
    });
  }
  function readPreferences() {
    void run("preferences", serviceFixture.preferences, (snapshot) => {
      setPreferences(snapshot.data); setDraft(snapshot.data);
    });
  }
  function savePreferences() {
    if (!preferences || !draft) return;
    const submitted = { ...draft };
    const request = {
      expected_revision: preferences.revision, idempotency_key: crypto.randomUUID(),
      patch: { ui_locale: submitted.ui_locale, close_to_tray: submitted.close_to_tray, hotkey: submitted.hotkey },
    };
    void run("preferences", () => serviceFixture.updatePreferences(request), (snapshot) => {
      const updated = { ...submitted, revision: snapshot.data.revision };
      setPreferences(updated); setDraft(updated);
    });
  }
  function rename() {
    if (!meeting) return;
    const original = meeting;
    const request = { meeting_id: original.id, expected_revision: original.metadata_revision, idempotency_key: crypto.randomUUID(), title };
    void run("rename", () => serviceFixture.renameMeeting(request), (snapshot) => {
      setMeeting({ ...original, title: request.title, metadata_revision: snapshot.data.revision, title_auto: false });
      // A mutation invalidates the list snapshot and its continuation.
      invalidate("meetings"); setMeetings(null);
    });
  }
  function prepareDelete() {
    if (!meeting) return;
    setDeleteRequest({
      meeting_id: meeting.id,
      expected_lifecycle_revision: meeting.lifecycle_revision,
      idempotency_key: crypto.randomUUID(),
    });
  }
  function removeMeeting() {
    if (!deleteRequest) return;
    void run("remove", () => serviceFixture.deleteMeeting(deleteRequest), clearPages);
  }
  function readTranscript(next: boolean) {
    if (!meeting) return;
    const request = {
      meeting_id: meeting.id, source_id: null, expected_revision: meeting.transcript_revision,
      representation, cursor: next ? transcript?.next ?? null : null, limit: PAGE_LIMIT, max_bytes: PAGE_BYTES,
    };
    void run("transcript", () => serviceFixture.transcript(request), (page) => {
      if (page.state.meeting_id === request.meeting_id && page.state.revision === request.expected_revision && page.representation === request.representation) setTranscript(page);
      else fail({ code: "RESYNC_REQUIRED", message: "The transcript context changed. Reconnect to continue.", retryable: false });
    });
  }
  function readSummary(next: boolean) {
    if (!meeting) return;
    const request = { meeting_id: meeting.id, expected_revision: meeting.summary_revision, field, cursor: next ? summary?.next ?? null : null, max_bytes: PAGE_BYTES };
    void run("summary", () => serviceFixture.summary(request), (page) => {
      if (page.state.meeting_id === request.meeting_id && page.state.revision === request.expected_revision && page.field === request.field) setSummary(page);
      else fail({ code: "RESYNC_REQUIRED", message: "The summary context changed. Reconnect to continue.", retryable: false });
    });
  }
  function readChanges(next: boolean) {
    const cursor = next ? changes?.next ?? null : null;
    void run("changes", () => serviceFixture.changes({ cursor, limit: PAGE_LIMIT, max_bytes: PAGE_BYTES }), (page) => {
      if (page.items.length > 0) {
        invalidate("meetings"); setMeetings(null); clearSelection();
        invalidate("preferences"); setPreferences(null); setDraft(null);
      }
      setChanges(page);
    });
  }

  const buttonClass = "rounded border border-line px-3 py-1.5 disabled:opacity-40";
  const inputClass = "rounded border border-line bg-surface px-2 py-1";
  return <main data-testid="service-fixture" className="h-full overflow-auto bg-surface p-6 text-ink">
    <div className="mx-auto flex max-w-5xl flex-col gap-6">
      <header>
        <h1 className="text-xl font-semibold">Experimental service fixture</h1>
        <p className="text-sm text-muted">Isolated local fixture. One bounded page per read; preferences are stored only in this fixture.</p>
      </header>
      {!available ? <p role="alert">Service unavailable outside the desktop fixture window.</p> : <>
        <section aria-label="Connection" className="flex flex-wrap items-center gap-3">
          <button className={buttonClass} disabled={connecting} onClick={() => void connect(true)}>Reconnect</button>
          {connecting && <span role="status">Connecting…</span>}
          {identity && <p className="break-all text-sm">Library {identity.library_id} · epoch {identity.library_epoch}</p>}
        </section>
        {error && <p role="alert">{error.code}: {error.message}</p>}
        <CaptureControls canStart={identity !== null} onChanged={clearPages} />
        <fieldset disabled={!identity} className="flex flex-col gap-6">
          <section aria-label="Fixture preferences" className="flex flex-col gap-3">
            <h2 className="font-semibold">Fixture preferences</h2>
            <div><button className={buttonClass} disabled={busy.preferences} onClick={readPreferences}>Refresh preferences</button></div>
            {preferences && draft && <>
              <p className="break-all text-sm">Revision: {preferences.revision}</p>
              <div className="flex flex-wrap items-center gap-4">
                <label>Locale <select aria-label="Fixture locale" className={inputClass} value={draft.ui_locale} disabled={busy.preferences} onChange={(event) => setDraft({ ...draft, ui_locale: event.target.value as Preferences["ui_locale"] })}><option value="en">English</option><option value="ru">Русский</option></select></label>
                <label><input type="checkbox" checked={draft.close_to_tray} disabled={busy.preferences} onChange={(event) => setDraft({ ...draft, close_to_tray: event.target.checked })} /> Close to tray</label>
                <label>Hotkey <input aria-label="Fixture hotkey" className={inputClass} maxLength={256} value={draft.hotkey} disabled={busy.preferences} onChange={(event) => setDraft({ ...draft, hotkey: event.target.value })} /></label>
                <button className={buttonClass} disabled={busy.preferences || !draft.hotkey} onClick={savePreferences}>Save preferences</button>
              </div>
            </>}
          </section>
          <section aria-label="Meeting pages" className="flex flex-col gap-3">
            <h2 className="font-semibold">Meeting pages</h2>
            <div className="flex flex-wrap items-center gap-3">
              <label>Kind <select aria-label="Content kind" className={inputClass} value={filter.content_kind ?? ""} onChange={(event) => changeFilter({ ...filter, content_kind: event.target.value as MeetingFilter["content_kind"] || null })}><option value="">All</option><option value="recorded">Recorded</option><option value="text">Text</option></select></label>
              <label>Folder <select aria-label="Folder filter" className={inputClass} value={filter.folder.kind} onChange={(event) => changeFilter({ ...filter, folder: event.target.value === "subtree" ? { kind: "subtree", folder_id: "" } : { kind: event.target.value as "all" | "unfiled" } })}><option value="all">All</option><option value="unfiled">Unfiled</option><option value="subtree">Subtree</option></select></label>
              {filter.folder.kind === "subtree" && <label>Folder ID <input className={inputClass} value={filter.folder.folder_id} maxLength={128} onChange={(event) => changeFilter({ ...filter, folder: { kind: "subtree", folder_id: event.target.value } })} /></label>}
              <label>Tag ID <input className={inputClass} value={filter.tag_id ?? ""} maxLength={128} onChange={(event) => changeFilter({ ...filter, tag_id: event.target.value || null })} /></label>
            </div>
            <div className="flex gap-3">
              <button className={buttonClass} disabled={busy.meetings || (filter.folder.kind === "subtree" && !filter.folder.folder_id)} onClick={() => readMeetings(false)}>Read meetings</button>
              <button className={buttonClass} disabled={busy.meetings || !meetings?.next} onClick={() => readMeetings(true)}>Next meeting page</button>
            </div>
            {meetings && <>
              <p className="text-sm">Snapshot {meetings.snapshot_sequence} · {meetings.items.length} items on this page</p>
              <ul className="flex flex-col gap-1">{meetings.items.map((item) => <li key={item.id} data-meeting-id={item.id}><button className={buttonClass} aria-pressed={selected === item.id} onClick={() => selectMeeting(item.id)}>{item.title}</button> <span className="text-sm text-muted">{item.content_kind} · {item.id}</span></li>)}</ul>
            </>}
          </section>
          {selected && <section aria-label="Meeting header" className="flex flex-col gap-3">
            <h2 className="font-semibold">Meeting header</h2>
            <div><button className={buttonClass} disabled={busy.meeting || busy.rename} onClick={() => selectMeeting(selected)}>Refresh meeting</button></div>
            {busy.meeting && <p role="status">Loading meeting…</p>}
            {meeting && <>
              <p className="break-all text-sm">{meeting.id} · metadata {meeting.metadata_revision} · transcript {meeting.transcript_revision} · summary {meeting.summary_revision}</p>
              <p className="text-sm">{meeting.content_kind} · audio {String(meeting.media.audio)} · video {String(meeting.media.video)}</p>
              <div className="flex gap-3"><label>Title <input className={inputClass} maxLength={512} value={title} disabled={busy.rename} onChange={(event) => setTitle(event.target.value)} /></label><button className={buttonClass} disabled={busy.rename || !title} onClick={rename}>Rename meeting</button></div>
              {deleteRequest
                ? <div className="flex gap-3">
                    <button className={buttonClass} disabled={busy.remove} onClick={removeMeeting}>Confirm delete</button>
                    <button className={buttonClass} disabled={busy.remove} onClick={() => setDeleteRequest(null)}>Cancel delete</button>
                  </div>
                : <div><button className={buttonClass} disabled={meeting.status === "recording" || meeting.status === "processing"} onClick={prepareDelete}>Delete meeting</button></div>}
            </>}
          </section>}
          {meeting && <>
            <section aria-label="Transcript fragments" className="flex flex-col gap-3">
              <h2 className="font-semibold">Transcript fragments</h2>
              <div className="flex flex-wrap items-center gap-3">
                <label>Representation <select className={inputClass} value={representation} onChange={(event) => { invalidate("transcript"); setTranscript(null); setRepresentation(event.target.value as Representation); }}><option value="raw">Raw</option><option value="effective">Effective</option></select></label>
                <button className={buttonClass} disabled={busy.transcript} onClick={() => readTranscript(false)}>Read transcript</button>
                <button className={buttonClass} disabled={busy.transcript || !transcript?.next} onClick={() => readTranscript(true)}>Next transcript page</button>
              </div>
              {transcript && <div data-testid="transcript-page">
                <p className="text-sm">{transcript.state.document_state} · source {transcript.source_id ?? "none"}</p>
                {transcript.items.map((item) => <article key={`${item.segment_index}:${item.byte_offset}`} className="my-3 rounded border border-line p-3">
                  <p className="text-sm text-muted">Segment {item.segment_id} · {item.start_ms}–{item.end_ms} ms · byte {item.byte_offset}/{item.total_bytes} · effective origin {item.effective_origin}</p>
                  <pre className="whitespace-pre-wrap break-words">{item.text}</pre>
                </article>)}
              </div>}
            </section>
            <section aria-label="Summary fragments" className="flex flex-col gap-3">
              <h2 className="font-semibold">Summary fragments</h2>
              <div className="flex flex-wrap items-center gap-3">
                <label>Field <select className={inputClass} value={field} onChange={(event) => { invalidate("summary"); setSummary(null); setField(event.target.value as SummaryField); }}><option value="markdown">Markdown</option><option value="structured_json">Structured JSON</option></select></label>
                <button className={buttonClass} disabled={busy.summary} onClick={() => readSummary(false)}>Read summary</button>
                <button className={buttonClass} disabled={busy.summary || !summary?.next} onClick={() => readSummary(true)}>Next summary page</button>
              </div>
              {summary && <div data-testid="summary-page"><p className="text-sm">{summary.state.document_state} · byte {summary.byte_offset}/{summary.total_bytes} · {summary.state.origin}</p><pre className="whitespace-pre-wrap break-words">{summary.text}</pre></div>}
            </section>
          </>}
          <section aria-label="Change pages" className="flex flex-col gap-3">
            <h2 className="font-semibold">Change pages</h2>
            <div className="flex gap-3"><button className={buttonClass} disabled={busy.changes} onClick={() => readChanges(false)}>Read changes</button><button className={buttonClass} disabled={busy.changes || !changes?.has_more} onClick={() => readChanges(true)}>Next change page</button></div>
            {changes && <><p className="text-sm">Current sequence {changes.current_sequence}</p><ul>{changes.items.map((item) => <li key={item.sequence}>{item.sequence} · {item.entity_kind} · {item.entity_id} · {item.revision} · {item.actor}</li>)}</ul></>}
          </section>
        </fieldset>
      </>}
    </div>
  </main>;
}
