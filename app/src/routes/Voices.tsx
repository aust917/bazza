import { displayError } from "@/lib/i18n";
import { useTranslation } from "react-i18next";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { AudioLines, FileAudio, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Button, Card, Input, SectionTitle, Select, Spinner } from "@/components/ui";
import { fmtDuration } from "@/lib/format";
import { cmd, type Meeting, type VoiceProfile, type VoiceSample, type VoiceSampleSource } from "@/lib/ipc";

class VoiceValidationError extends Error {
  constructor(readonly key: "voices.durationError" | "voices.fileError" | "voices.recordingError") { super(key); }
}

export function VoicesPage() {
  const { t: tr } = useTranslation("main");
  const [profiles, setProfiles] = useState<VoiceProfile[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [samples, setSamples] = useState<VoiceSample[]>([]);
  const [meetings, setMeetings] = useState<Meeting[]>([]);
  const [name, setName] = useState("");
  const [newName, setNewName] = useState("");
  const [kind, setKind] = useState<"file" | "meeting">("file");
  const [file, setFile] = useState<string | null>(null);
  const [meetingId, setMeetingId] = useState("");
  const [start, setStart] = useState("0");
  const [end, setEnd] = useState("10");
  const [busy, setBusy] = useState(false);
  const [loadingSamples, setLoadingSamples] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [audio, setAudio] = useState<{ id: string; url: string } | null>(null);

  const profile = profiles?.find((p) => p.id === selected);
  const reload = async () => setProfiles(await cmd.listVoiceProfiles());
  useEffect(() => {
    void cmd.listVoiceProfiles().then(setProfiles).catch((e) => setError(e));
    void cmd.listMeetings().then((rows) => setMeetings(rows.filter((m) => m.duration_ms >= 1000 && m.status !== "recording"))).catch((e) => setError(e));
  }, []);

  useEffect(() => {
    let cancelled = false;
    setSamples([]);
    setAudio(null);
    if (!selected) { setLoadingSamples(false); return; }
    setLoadingSamples(true);
    void cmd.listVoiceSamples(selected).then((rows) => {
      if (!cancelled) setSamples(rows);
    }).catch((e) => { if (!cancelled) setError(e); })
      .finally(() => { if (!cancelled) setLoadingSamples(false); });
    return () => { cancelled = true; };
  }, [selected]);

  useEffect(() => setName(profile?.name ?? ""), [profile?.name, selected]);

  const run = async (action: () => Promise<void>) => {
    setError(null);
    setBusy(true);
    try { await action(); }
    catch (e) { setError(e); }
    finally { setBusy(false); }
  };

  const create = () => run(async () => {
    const p = await cmd.createVoiceProfile(newName.trim());
    await reload();
    setSelected(p.id);
    setNewName("");
  });

  const addSample = () => run(async () => {
    if (!selected) return;
    const a = Math.round(Number(start) * 1000);
    const b = Math.round(Number(end) * 1000);
    if (!start.trim() || !end.trim() || !Number.isSafeInteger(a) || !Number.isSafeInteger(b) || a < 0 || b - a < 1000 || b - a > 120_000) {
      throw new VoiceValidationError("voices.durationError");
    }
    let source: VoiceSampleSource;
    if (kind === "file") {
      if (!file) throw new VoiceValidationError("voices.fileError");
      source = { kind: "file", path: file, start_ms: a, end_ms: b };
    } else {
      if (!meetingId) throw new VoiceValidationError("voices.recordingError");
      source = { kind: "meeting", meeting_id: meetingId, start_ms: a, end_ms: b };
    }
    await cmd.addVoiceSample(selected, source);
    setSamples(await cmd.listVoiceSamples(selected));
    await reload();
  });

  const chooseFile = () => run(async () => {
    const path = await open({ multiple: false, title: tr("voices.fileTitle"), filters: [{ name: tr("library.mediaFilter"), extensions: ["wav", "mp3", "m4a", "aac", "flac", "ogg", "opus", "mp4", "mov", "mkv", "webm"] }] });
    if (typeof path === "string") setFile(path);
  });

  return (
    <div className="mx-auto max-w-[1200px] px-8 py-7">
      <div className="mb-6">
        <h1 className="font-sans font-semibold text-[30px] tracking-tight">{tr("nav.voices")}</h1>
        <p className="mt-1 max-w-2xl text-sm text-ink-3">{tr("voices.description")}</p>
        <p className="mt-1 max-w-2xl text-sm text-ink-3">{tr("voices.matchingHint")}</p>
        <p className="mt-1 text-[12px] text-ink-3">{tr("voices.privacy")}</p>
      </div>
      {error != null && <p role="alert" className="mb-4 rounded-md border border-ember/30 bg-ember-soft px-3 py-2 text-sm text-ember">{error instanceof VoiceValidationError ? tr(error.key) : displayError(error)}</p>}
      <div className="grid gap-6 lg:grid-cols-[260px_minmax(0,1fr)]">
        <div>
          <SectionTitle>{tr("voices.people")}</SectionTitle>
          <Card className="overflow-hidden">
            {profiles === null ? <div className="p-4"><Spinner /></div> : profiles.length === 0 ? <p className="p-4 text-sm text-ink-3">{tr("voices.empty")}</p> : (
              <ul className="divide-y divide-line">
                {profiles.map((p) => <li key={p.id}><button disabled={busy} className={`focus-ring w-full px-4 py-3 text-left ${selected === p.id ? "bg-canvas-3" : "hover:bg-canvas-2"}`} onClick={() => { setSelected(p.id); setError(null); }}>
                  <span className="block text-sm font-medium">{p.name}</span>
                  <span className="text-[12px] text-ink-3">{tr("voices.sampleCount", { count: p.sample_count })}{!p.has_voiceprint && tr("voices.noVoiceprint")}</span>
                </button></li>)}
              </ul>
            )}
          </Card>
          <form className="mt-3 flex gap-2" onSubmit={(e) => { e.preventDefault(); void create(); }}>
            <Input aria-label={tr("voices.newName")} placeholder={tr("voices.personName")} maxLength={200} value={newName} disabled={busy} onChange={(e) => setNewName(e.target.value)} />
            <Button type="submit" size="icon" disabled={busy || !newName.trim()} aria-label={tr("voices.addPerson")}><Plus size={16} /></Button>
          </form>
        </div>
        {profile ? (
          <div>
            <SectionTitle>{tr("voices.profile")}</SectionTitle>
            <Card className="p-4">
              <form className="flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); void run(async () => { await cmd.renameVoiceProfile(profile.id, name.trim()); await reload(); }); }}>
                <Input className="max-w-xs" aria-label={tr("voices.profileName")} maxLength={200} value={name} disabled={busy} onChange={(e) => setName(e.target.value)} />
                <Button type="submit" disabled={busy || !name.trim() || name.trim() === profile.name}>{tr("voices.saveName")}</Button>
                <Button variant="ghost" disabled={busy || !profile.sample_count} onClick={() => void run(async () => { await cmd.recomputeVoiceProfile(profile.id); await reload(); })}><RefreshCw size={14} /> {tr("voices.recompute")}</Button>
                <Button variant="danger" className="ml-auto" disabled={busy} onClick={() => {
                  if (confirm(tr("voices.deleteConfirm", { name: profile.name }))) void run(async () => { await cmd.deleteKnownSpeaker(profile.id); setSelected(null); await reload(); });
                }}><Trash2 size={14} /> {tr("voices.deleteProfile")}</Button>
              </form>
              <p className="mt-2 text-[12px] text-ink-3">{tr("voices.profileHint")}</p>
            </Card>

            <div className="mt-6"><SectionTitle>{tr("voices.samples")}</SectionTitle>
              {loadingSamples ? <Spinner /> : samples.length === 0 ? <p className="text-sm text-ink-3">{tr("voices.noSamples")}</p> : <ul className="space-y-2">
                {samples.map((s) => <li key={s.id}><Card className="px-4 py-3">
                  <div className="flex items-center gap-3">
                    <AudioLines size={18} className="shrink-0 text-ink-3" />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">{s.source_label || tr("voices.savedVoiceprint")}</div>
                      <div className="text-[12px] text-ink-3">{s.source === "legacy" ? tr("voices.legacy") : s.source === "meeting" ? tr("voices.fromMeeting") : tr("voices.imported")}{s.start_ms != null && s.end_ms != null && ` · ${fmtDuration(s.start_ms)}–${fmtDuration(s.end_ms)}`}{!s.has_audio && tr("voices.voiceprintOnly")}</div>
                    </div>
                    {s.has_audio && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(async () => { setAudio({ id: s.id, url: convertFileSrc(await cmd.voiceSampleAudio(s.id)) }); })}>{tr("voices.listen")}</Button>}
                    <Button size="iconSm" variant="danger" aria-label={tr("voices.deleteSample", { name: s.source_label || s.id })} disabled={busy} onClick={() => void run(async () => { await cmd.deleteVoiceSample(s.id); if (audio?.id === s.id) setAudio(null); setSamples(await cmd.listVoiceSamples(profile.id)); await reload(); })}><Trash2 size={14} /></Button>
                  </div>
                  {audio?.id === s.id && <audio aria-label={tr("voices.listen")} key={audio.url} controls autoPlay src={audio.url} className="mt-3 h-9 w-full" />}
                </Card></li>)}
              </ul>}
            </div>

            <form className="mt-6" onSubmit={(e) => { e.preventDefault(); void addSample(); }}>
              <SectionTitle>{tr("voices.addSpeech")}</SectionTitle>
              <Card className="space-y-4 p-4">
                <p className="text-[13px] text-ink-3">{tr("voices.sampleHint")} <Link to="/settings" className="underline">{tr("nav.settings")}</Link>.</p>
                <div className="grid gap-3 sm:grid-cols-2">
                  <label className="space-y-1 text-[12px] text-ink-3">{tr("voices.source")}<Select aria-label={tr("voices.sampleSource")} value={kind} disabled={busy} onChange={(e) => setKind(e.target.value as "file" | "meeting")}><option value="file">{tr("voices.audioFile")}</option><option value="meeting">{tr("voices.existingRecording")}</option></Select></label>
                  {kind === "file" ? <div className="flex min-w-0 items-end gap-2"><Button disabled={busy} onClick={() => void chooseFile()}><FileAudio size={14} /> {tr("voices.chooseFile")}</Button><span className="mb-2 truncate text-[12px] text-ink-3" title={file ?? undefined}>{file?.split(/[\\/]/).pop() ?? tr("voices.noFile")}</span></div> : <label className="space-y-1 text-[12px] text-ink-3">{tr("voices.recording")}<Select aria-label={tr("voices.sampleRecording")} value={meetingId} disabled={busy} onChange={(e) => setMeetingId(e.target.value)}><option value="">{tr("voices.chooseRecording")}</option>{meetings.map((m) => <option key={m.id} value={m.id}>{m.title} ({fmtDuration(m.duration_ms)})</option>)}</Select></label>}
                </div>
                <div className="flex flex-wrap items-end gap-3">
                  <label className="w-32 space-y-1 text-[12px] text-ink-3">{tr("voices.start")}<Input aria-label={tr("voices.startAria")} type="number" min="0" step="0.001" value={start} disabled={busy} onChange={(e) => setStart(e.target.value)} /></label>
                  <label className="w-32 space-y-1 text-[12px] text-ink-3">{tr("voices.end")}<Input aria-label={tr("voices.endAria")} type="number" min="1" step="0.001" value={end} disabled={busy} onChange={(e) => setEnd(e.target.value)} /></label>
                  <Button type="submit" variant="primary" disabled={busy}>{busy ? <Spinner /> : <Plus size={14} />} {tr("voices.addSample")}</Button>
                </div>
              </Card>
            </form>
          </div>
        ) : <div className="flex min-h-48 items-center justify-center text-sm text-ink-3">{tr("voices.choosePerson")}</div>}
      </div>
    </div>
  );
}
