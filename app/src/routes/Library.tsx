import { displayError } from "@/lib/i18n";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-dialog";
import { CalendarDays, Circle, FileAudio, FolderInput, Search, Video } from "lucide-react";
import { useCallback, useEffect, useEffectEvent, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link, useLocation, useNavigate } from "react-router-dom";
import { EmptyState } from "@/components/EmptyState";
import { FolderTree, type Selection } from "@/components/FolderTree";
import { Badge, Button, Input, Menu } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { appLabel, fmtDate, fmtDuration, fmtTime } from "@/lib/format";
import { cmd, subscribe, type CalEvent, type Events, type Folder, type Meeting, type SearchHit, type Tag } from "@/lib/ipc";
import { useLibraryDrag } from "./useLibraryDrag";

const STATUS_TONE = { recording: "ember", processing: "amber", ready: "moss", failed: "ember" } as const;
const IMPORT_EXTENSIONS = ["mp3", "m4a", "aac", "wav", "flac", "ogg", "opus", "wma", "aiff", "amr", "mp4", "mov", "mkv", "webm", "avi", "m4v"];

export function Library({ serviceDesktop = false }: { serviceDesktop?: boolean }) {
  const { t: tr } = useTranslation("main");
  const [meetings, setMeetings] = useState<Meeting[] | null>(null);
  const [folders, setFolders] = useState<Folder[]>([]);
  const [tags, setTags] = useState<Tag[]>([]);
  const [selection, setSelection] = useState<Selection>({ kind: "all" });
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  const [upcoming, setUpcoming] = useState<CalEvent[]>([]);
  const [importWork, setImportWork] = useState<{ file: string; completed: number; total: number; paired: boolean; startedAt: number } | null>(null);
  const choosingImport = useRef(false);
  const libraryRoot = useRef<HTMLDivElement>(null);
  const [dropError, setDropError] = useState<string | null>(null);
  const importing = importWork !== null;
  const [serviceError, setServiceError] = useState<string | null>(null);
  const [organizationError, setOrganizationError] = useState<string | null>(null);
  const [moveError, setMoveError] = useState<string | null>(null);
  const [movingIds, setMovingIds] = useState<ReadonlySet<string>>(() => new Set());
  const pendingMoves = useRef(new Set<string>());
  const moveRequests = useRef(new Map<string, { folderId: string | null; revision: string; key: string }>());
  const [edit, setEdit] = useState<{ meeting: Meeting; title: string } | null>(null);
  const [pending, setPending] = useState(false);
  const mutation = useRef<{ payload: string; key: string } | null>(null);
  const navigate = useNavigate();
  const location = useLocation();
  const meetingRequest = useRef(0);
  const metadataRequest = useRef(0);

  const openRecordingSetup = () => navigate("/recording", { state: { returnTo: location.pathname + location.search } });

  useEffect(() => {
    if (serviceDesktop) return;
    const load = () => void cmd.listUpcoming(12).then(setUpcoming).catch(() => setUpcoming([]));
    load();
    const t = setInterval(load, 60_000);
    return () => clearInterval(t);
  }, [serviceDesktop]);

  const reloadMeetings = useCallback(async () => {
    const request = ++meetingRequest.current;
    const filter =
      selection.kind === "all" ? { folder: null, tag_id: null }
      : selection.kind === "unfiled" ? { folder: [null], tag_id: null }
      : selection.kind === "folder" ? { folder: [selection.id], tag_id: null }
      : { folder: null, tag_id: selection.id };
    try {
      const rows = serviceDesktop && selection.kind === "all" ? await cmd.listMeetings()
        : await cmd.listMeetingsFiltered(filter as { folder: null | (string | null)[]; tag_id: string | null });
      if (request === meetingRequest.current) { setMeetings(rows); setServiceError(null); }
    } catch (error) {
      if (serviceDesktop && request === meetingRequest.current) setServiceError(displayError(error));
    }
  }, [selection, serviceDesktop]);
  const reloadMetadata = useCallback(async () => {
    const request = ++metadataRequest.current;
    try {
      const [nextFolders, nextTags] = serviceDesktop
        ? await cmd.serviceLibraryOrganization().then(({ folders, tags }) => [folders, tags] as const)
        : await Promise.all([cmd.listFolders(), cmd.listTags()]);
      if (request === metadataRequest.current) {
        setFolders(nextFolders);
        setTags(nextTags);
        setOrganizationError(null);
      }
    } catch (error) {
      if (serviceDesktop && request === metadataRequest.current) setOrganizationError(displayError(error));
    }
  }, [serviceDesktop]);
  const reload = useCallback(async () => {
    await Promise.allSettled([reloadMeetings(), reloadMetadata()]);
  }, [reloadMeetings, reloadMetadata]);
  const reloadCurrent = useRef(reload);
  useLayoutEffect(() => { reloadCurrent.current = reload; }, [reload]);

  // This measures import requests. Meeting processing continues in the background.
  const importFiles = async (paths: string[], transcript?: string) => {
    const startedAt = performance.now();
    const paired = transcript !== undefined;
    const failed: string[] = [];
    let last: string | null = null;
    for (const [completed, path] of paths.entries()) {
      const file = path.split(/[\\/]/).pop() ?? path;
      setImportWork({ file, completed, total: paths.length, paired, startedAt });
      try {
        last = (await (transcript ? cmd.importRecording(path, undefined, transcript) : cmd.importRecording(path))).id;
      } catch (error) {
        if (paired) throw error;
        failed.push(`${file}: ${displayError(error)}`);
      }
    }
    return { last, failed };
  };

  const importRecordings = async (paired: boolean) => {
    if (choosingImport.current) return;
    choosingImport.current = true;
    setDropError(null);
    try {
      const picked = await open({
        multiple: !paired,
        title: tr(paired ? "library.chooseRecording" : "library.importRecording"),
        filters: [{ name: tr("library.mediaFilter"), extensions: IMPORT_EXTENSIONS }],
      });
      const paths = Array.isArray(picked) ? picked : picked ? [picked] : [];
      if (paths.length === 0) return;
      let transcript: string | undefined;
      if (paired) {
        const pickedTranscript = await open({ multiple: false, title: tr("library.chooseTranscript"), filters: [{ name: tr("library.subtitleFilter"), extensions: ["srt", "vtt"] }] });
        if (typeof pickedTranscript !== "string") return;
        transcript = pickedTranscript;
      }
      const { last, failed } = await importFiles(paths, transcript);
      void reloadCurrent.current();
      if (failed.length) alert(tr("library.importFailed", { details: failed.join("\n") }));
      if (paths.length === 1 && last) navigate(`/meeting/${last}`);
    } catch (error) {
      alert(tr(paired ? "library.pairFailed" : "library.importFailed", { details: displayError(error) }));
    } finally {
      choosingImport.current = false;
      setImportWork(null);
    }
  };

  const importDrop = useEffectEvent(async ({ paths, position }: Events["tauri://drag-drop"]) => {
    const scale = window.devicePixelRatio;
    const target = document.elementFromPoint(position.x / scale, position.y / scale);
    if (!target || !libraryRoot.current?.contains(target)) return;
    if (choosingImport.current) {
      setDropError(tr("library.dropBusy"));
      return;
    }

    const media: string[] = [];
    const unsupported: string[] = [];
    for (const path of new Set(paths)) {
      const extension = path.match(/\.([^.\\/]+)$/)?.[1]?.toLowerCase();
      if (extension && IMPORT_EXTENSIONS.includes(extension)) media.push(path);
      else unsupported.push(path.split(/[\\/]/).pop() ?? path);
    }
    if (paths.length === 0) return;
    setDropError(null);
    choosingImport.current = true;
    try {
      const { last, failed } = await importFiles(media);
      if (last) {
        setSelection((current) => current.kind === "all" ? current : { kind: "all" });
        setQuery("");
        setHits(null);
        void reloadCurrent.current();
      }
      if (unsupported.length) failed.push(tr("library.dropUnsupported", { files: unsupported.join(", ") }));
      if (failed.length) setDropError(tr("library.importFailed", { details: failed.join("\n") }));
    } finally {
      choosingImport.current = false;
      setImportWork(null);
    }
  });

  useEffect(() => {
    if (serviceDesktop) return;
    return subscribe("tauri://drag-drop", (drop) => void importDrop(drop));
  }, [serviceDesktop]);

  const moveMeeting = async (id: string, folderId: string | null) => {
    const meeting = meetings?.find((row) => row.id === id);
    if (!meeting || meeting.folder_id === folderId || pendingMoves.current.has(id)) return;
    const revision = meeting.metadata_revision;
    if (serviceDesktop && !revision) return;
    pendingMoves.current.add(id);
    setMovingIds(new Set(pendingMoves.current));
    setMoveError(null);
    try {
      if (serviceDesktop && revision) {
        let request = moveRequests.current.get(id);
        if (!request || request.folderId !== folderId || request.revision !== revision) {
          request = { folderId, revision, key: crypto.randomUUID() };
          moveRequests.current.set(id, request);
        }
        await cmd.mutateServiceLibraryOrganization({
          op: "meeting_move", meeting_id: id, folder_id: folderId, expected_revision: revision, idempotency_key: request.key,
        });
        moveRequests.current.delete(id);
      } else {
        await cmd.moveMeeting(id, folderId);
      }
      await reloadCurrent.current();
    } catch (error) {
      setMoveError(tr("library.moveFailed", { error: displayError(error) }));
      if (serviceDesktop) await reloadCurrent.current();
    } finally {
      pendingMoves.current.delete(id);
      setMovingIds(new Set(pendingMoves.current));
    }
  };
  const drag = useLibraryDrag((id, folderId) => { void moveMeeting(id, folderId); });

  useEffect(() => {
    void reloadMetadata();
    return () => { metadataRequest.current++; };
  }, [reloadMetadata]);

  useEffect(() => {
    let active = true, running = false, pending = false, metadata = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stages = new Map<string, string>();
    const schedule = (includeMetadata = false) => {
      pending = true;
      metadata ||= includeMetadata;
      if (active && !running && timer === undefined) timer = setTimeout(() => void refresh(), 100);
    };
    const refresh = async () => {
      timer = undefined;
      if (!active) return;
      running = true;
      pending = false;
      const includeMetadata = metadata;
      metadata = false;
      try {
        await Promise.all([reloadMeetings(), includeMetadata ? reloadMetadata() : undefined]);
      } catch { /* Keep the last available library while a refresh is unavailable. */ }
      finally {
        running = false;
        if (active && pending) schedule();
      }
    };
    void refresh();
    // Percent updates do not change a library row. Refresh on stage changes,
    // merging bursts and allowing at most one event-driven refresh at a time.
    const a = subscribe("recording_state", () => schedule(true));
    const b = subscribe("processing", (p) => {
      if (stages.get(p.meeting_id) === p.stage) return;
      stages.set(p.meeting_id, p.stage);
      schedule();
    });
    return () => {
      active = false;
      clearTimeout(timer);
      meetingRequest.current++;
      a();
      b();
    };
  }, [reloadMeetings, reloadMetadata]);

  useEffect(() => {
    if (serviceDesktop) return;
    const q = query.trim();
    if (q.length < 2) {
      setHits(null);
      return;
    }
    let active = true;
    const t = setTimeout(() => void cmd.search(q).then((rows) => { if (active) setHits(rows); }).catch(() => {}), 120);
    return () => { active = false; clearTimeout(t); };
  }, [query, serviceDesktop]);

  const heading =
    selection.kind === "all" ? tr("nav.library")
    : selection.kind === "unfiled" ? tr("folders.unfiled")
    : selection.kind === "folder" ? (folders.find((f) => f.id === selection.id)?.name ?? tr("folders.folder"))
    : `#${tags.find((t) => t.id === selection.id)?.name ?? tr("folders.tag")}`;

  const folderDestinations = useMemo(() => {
    const byId = new Map(folders.map((folder) => [folder.id, folder]));
    return folders.map((folder) => {
      const path = [folder.name];
      const visited = new Set([folder.id]);
      let parentId = folder.parent_id;
      while (parentId && !visited.has(parentId)) {
        const parent = byId.get(parentId);
        if (!parent) break;
        visited.add(parentId);
        path.unshift(parent.name);
        parentId = parent.parent_id;
      }
      return { id: folder.id, label: path.join(" / ") };
    });
  }, [folders]);

  const changeMeeting = async (meeting: Meeting, title?: string) => {
    const revision = title === undefined ? meeting.lifecycle_revision : meeting.metadata_revision;
    if (!revision || pending || title?.trim() === "") return;
    const payload = JSON.stringify([meeting.id, revision, title]);
    if (mutation.current?.payload !== payload) mutation.current = { payload, key: crypto.randomUUID() };
    setPending(true);
    setServiceError(null);
    try {
      if (title === undefined) await cmd.deleteMeeting(meeting.id, revision, mutation.current.key);
      else await cmd.renameMeeting(meeting.id, title, revision, mutation.current.key);
      mutation.current = null;
      setEdit(null);
      await reloadMeetings();
    } catch (error) {
      setServiceError(displayError(error));
    } finally {
      setPending(false);
    }
  };

  return (
    <div ref={libraryRoot} className="grid h-full grid-cols-[220px_minmax(0,1fr)]" {...drag.handlers}>
      <aside className="overflow-y-auto border-r border-line px-3 py-6">
        <FolderTree
          folders={folders}
          tags={tags}
          selection={selection}
          onSelect={setSelection}
          onChange={reload}
          onError={setMoveError}
          serviceDesktop={serviceDesktop}
          dragTarget={drag.preview?.target}
        />
      </aside>

      <div className="mx-auto w-full max-w-4xl px-8 py-8">
        <header className="mb-6 flex flex-wrap items-end justify-between gap-4">
          <div>
            <h1 className="font-sans font-semibold text-[34px] leading-none tracking-tight">{heading}</h1>
            <p className="mt-1.5 text-[13px] text-ink-3">
              {meetings ? tr("library.recordingCount", { count: meetings.length }) : tr("library.loading")}
            </p>
          </div>
          {!serviceDesktop && <div className="flex flex-wrap items-center gap-2">
            <Button variant="outline" size="sm" onClick={() => void importRecordings(false)} disabled={importing} title={tr("library.importHint")}>
              <FileAudio size={14} /> {importing ? tr("library.importing") : tr("library.import")}
            </Button>
            <Button variant="outline" size="sm" onClick={() => void importRecordings(true)} disabled={importing} title={tr("library.pairHint")}>
              {tr("library.pair")}
            </Button>
            <div className="relative min-w-48 flex-1">
              <Search size={14} className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-ink-3" />
              <Input placeholder={tr("library.search")} value={query} onChange={(e) => setQuery(e.target.value)} className="pl-8" />
            </div>
          </div>}
        </header>
        {serviceError && <div role="alert" className="mb-4 flex items-center gap-2 text-sm text-ember">
          {serviceError}<Button variant="outline" size="sm" onClick={() => { setEdit(null); reload(); }}>{tr("library.refresh")}</Button>
        </div>}
        {organizationError && <div role="alert" className="mb-4 flex items-center gap-2 text-sm text-ember">
          {organizationError}<Button variant="outline" size="sm" onClick={() => void reloadMetadata()}>{tr("library.refresh")}</Button>
        </div>}
        {moveError && <div role="alert" className="mb-4 text-sm text-ember">{moveError}</div>}
        {dropError && <div role="alert" className="mb-4 whitespace-pre-line text-sm text-ember">{dropError}</div>}
        {importWork && <OperationProgress
          label={tr(importWork.paired ? "library.importPairProgress" : "library.importProgress")}
          className="mb-4 max-w-sm"
          announceStage
          value={importWork.total > 1 ? importWork.completed / importWork.total : undefined}
          startedAt={importWork.startedAt}
          detail={<>
            {importWork.total > 1 && <span>{tr("library.importProcessed", { done: importWork.completed, total: importWork.total })} · </span>}
            <span className="break-all">{importWork.file}</span>
          </>}
        />}

        {!hits && selection.kind === "all" && upcoming.length > 0 && (
          <section className="mb-6">
            <div className="mb-2 flex items-center gap-1.5 text-[11px] font-semibold tracking-wide text-ink-3 uppercase"><CalendarDays size={12} /> {tr("library.today")}</div>
            <ul className="flex flex-col gap-1">
              {upcoming.slice(0, 4).map((e) => {
                const start = new Date(e.start).getTime();
                const live = start - 10 * 60_000 <= Date.now() && Date.now() <= new Date(e.end).getTime();
                return (
                  <li key={`${e.uid}-${e.start}`} className="flex items-center gap-3 rounded-md border border-line px-3 py-2 text-[13px]">
                    <span className="w-14 shrink-0 font-mono text-[12px] text-ink-3">{fmtTime(e.start)}</span>
                    <span className="min-w-0 flex-1 truncate font-medium">{e.title}</span>
                    {e.attendees.length > 0 && <span className="hidden truncate text-[12px] text-ink-3 md:inline">{e.attendees.slice(0, 3).join(", ")}{e.attendees.length > 3 ? ` +${e.attendees.length - 3}` : ""}</span>}
                    {live ? (
                      <Button variant="ember" size="sm" onClick={openRecordingSetup}><Circle size={10} fill="currentColor" /> {tr("library.record")}</Button>
                    ) : (
                      <span className="text-[12px] text-ink-3">{tr("library.inMinutes", { count: Math.max(1, Math.round((start - Date.now()) / 60_000)) })}</span>
                    )}
                  </li>
                );
              })}
            </ul>
          </section>
        )}

        {hits ? (
          <ul className="flex flex-col gap-1">
            {hits.length === 0 && <li className="py-10 text-center text-sm text-ink-3">{tr("library.noMatches")}</li>}
            {hits.map((h) => (
              <li key={`${h.meeting_id}-${h.segment_id}`}>
                <button
                  className="focus-ring flex w-full items-baseline gap-4 rounded-md px-3 py-2 text-left hover:bg-canvas-2"
                  onClick={() => navigate(`/meeting/${h.meeting_id}${h.segment_id ? `?t=${h.start_ms}` : ""}`)}
                >
                  <span className="w-44 shrink-0 truncate text-[13px] font-medium">{h.meeting_title}</span>
                  <span className="w-12 shrink-0 font-mono text-[11px] text-ink-3">{h.segment_id ? fmtDuration(h.start_ms) : ""}</span>
                  <span className="truncate text-[13px] text-ink-2">{renderSnippet(h.snippet)}</span>
                </button>
              </li>
            ))}
          </ul>
        ) : meetings && meetings.length === 0 ? (
          selection.kind === "all" ? (
            <EmptyState
              title={tr("library.emptyTitle")}
              body={tr(serviceDesktop ? "library.serviceEmptyBody" : "library.emptyBody")}
              action={<div className="flex items-center gap-2"><Button variant="primary" onClick={openRecordingSetup}>{tr("shell.recordNow")}</Button>{!serviceDesktop && <Button variant="outline" disabled={importing} onClick={() => void importRecordings(false)}><FileAudio size={14} /> {tr("library.importFile")}</Button>}</div>}
            />
          ) : (
            <EmptyState title={tr("library.nothingHere")} body={tr("library.folderEmpty")} />
          )
        ) : (
          <ul className="flex flex-col">
            {meetings?.map((m, i) => {
              const canMove = !serviceDesktop || Boolean(m.metadata_revision);
              const rowClass = "focus-ring group flex min-w-0 flex-1 items-center gap-4 rounded-lg px-3 py-3.5 transition-colors hover:bg-canvas-2";
              const row = <>
                  <div className="flex w-16 shrink-0 flex-col text-[12px] leading-tight text-ink-3">
                    <span className="font-medium text-ink-2">{fmtDate(m.started_at)}</span>
                    <span>{fmtTime(m.started_at)}</span>
                  </div>
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[15px] font-medium text-ink group-hover:underline group-hover:decoration-line-2 group-hover:underline-offset-4">
                      {m.title}
                    </div>
                    <div className="mt-0.5 flex items-center gap-2 text-[12px] text-ink-3">
                      <span>{appLabel(m.app)}</span>
                      {m.folder_id && selection.kind !== "folder" && <span>{tr("library.inFolder", { name: folders.find((f) => f.id === m.folder_id)?.name ?? tr("folders.folder") })}</span>}
                      {m.has_video && <span className="inline-flex items-center gap-1"><Video size={12} /> {tr("library.video")}</span>}
                    </div>
                  </div>
                  <Badge tone={STATUS_TONE[m.status]}>{tr(`status.${m.status}`)}</Badge>
                  <span className="w-14 text-right font-mono text-[12px] tabular-nums text-ink-2">{fmtDuration(m.duration_ms)}</span>
              </>;
              return (
              <li
                key={m.id}
                className="rise relative has-[[aria-expanded=true]]:z-30"
                style={{ animationDelay: `${Math.min(i, 12) * 25}ms` }}
              >
                <div className={`flex items-center gap-1 border-b border-line ${serviceDesktop ? "flex-wrap" : ""}`}>
                  <Link
                    to={`/meeting/${m.id}`}
                    draggable={false}
                    onPointerDown={(event) => {
                      if (canMove && !movingIds.has(m.id)) drag.begin(event, m.id, m.folder_id);
                    }}
                    className={`${rowClass} ${drag.preview?.meetingId === m.id ? "cursor-grabbing opacity-60" : canMove ? "cursor-grab" : ""}`}
                  >{row}</Link>
                  {(folderDestinations.length > 0 || m.folder_id !== null) && <Menu
                    className="shrink-0 [&>[role=menu]]:max-h-64 [&>[role=menu]]:max-w-72 [&>[role=menu]]:overflow-y-auto [&_[role=menuitem]]:[overflow-wrap:anywhere]"
                    disabled={!canMove || movingIds.has(m.id)}
                    label={<span className="inline-flex" title={tr("library.moveToFolder")}>
                      <FolderInput size={16} aria-hidden="true" />
                      <span className="sr-only">{tr("library.moveToFolder")} · {m.title}</span>
                    </span>}
                    items={[{ id: null, label: tr("folders.unfiled") }, ...folderDestinations].map((destination) => ({
                      label: destination.label,
                      disabled: destination.id === m.folder_id,
                      onSelect: () => void moveMeeting(m.id, destination.id),
                    }))}
                  />}
                  {serviceDesktop && (edit?.meeting.id === m.id ? <form className="flex items-center gap-2" onSubmit={(event) => {
                    event.preventDefault();
                    void changeMeeting(edit.meeting, edit.title);
                  }}>
                    <Input aria-label={tr("actions.rename")} value={edit.title} disabled={pending} onChange={(event) => setEdit({ ...edit, title: event.target.value })} />
                    <Button size="sm" type="submit" disabled={pending || !edit.title.trim()}>{tr("actions.rename")}</Button>
                    <Button size="sm" type="button" variant="ghost" disabled={pending} onClick={() => setEdit(null)}>{tr("actions.back")}</Button>
                  </form> : <>
                    <Button size="sm" variant="ghost" disabled={pending || !m.metadata_revision} onClick={() => setEdit({ meeting: m, title: m.title })}>{tr("actions.rename")}</Button>
                    <Button size="sm" variant="ghost" disabled={pending || !m.lifecycle_revision || m.status === "recording" || m.status === "processing"}
                      onClick={() => { if (window.confirm(tr("library.deleteConfirm", { title: m.title }))) void changeMeeting(m); }}>{tr("actions.delete")}</Button>
                  </>)}
                </div>
              </li>
            ); })}
          </ul>
        )}
      </div>
    </div>
  );
}

/** FTS5 marks matches with <b>..</b>; render them as React nodes so nothing is injected as HTML. */
function renderSnippet(snippet: string) {
  return snippet.split(/(<b>.*?<\/b>)/g).map((part, i) =>
    part.startsWith("<b>") ? (
      <b key={i} className="font-semibold text-ink">
        {part.slice(3, -4)}
      </b>
    ) : (
      <span key={i}>{part}</span>
    ),
  );
}
