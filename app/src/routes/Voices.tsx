import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { AudioLines, FileAudio, Plus, RefreshCw, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { Button, Card, Input, SectionTitle, Select, Spinner } from "@/components/ui";
import { fmtDuration } from "@/lib/format";
import { cmd, type Meeting, type VoiceProfile, type VoiceSample, type VoiceSampleSource } from "@/lib/ipc";

export function VoicesPage() {
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
  const [error, setError] = useState<string | null>(null);
  const [audio, setAudio] = useState<{ id: string; url: string } | null>(null);

  const profile = profiles?.find((p) => p.id === selected);
  const reload = async () => setProfiles(await cmd.listVoiceProfiles());
  useEffect(() => {
    void cmd.listVoiceProfiles().then(setProfiles).catch((e) => setError(String(e)));
    void cmd.listMeetings().then((rows) => setMeetings(rows.filter((m) => m.duration_ms >= 1000 && m.status !== "recording"))).catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    let cancelled = false;
    setSamples([]);
    setAudio(null);
    if (!selected) { setLoadingSamples(false); return; }
    setLoadingSamples(true);
    void cmd.listVoiceSamples(selected).then((rows) => {
      if (!cancelled) setSamples(rows);
    }).catch((e) => { if (!cancelled) setError(String(e)); })
      .finally(() => { if (!cancelled) setLoadingSamples(false); });
    return () => { cancelled = true; };
  }, [selected]);

  useEffect(() => setName(profile?.name ?? ""), [profile?.name, selected]);

  const run = async (action: () => Promise<void>) => {
    setError(null);
    setBusy(true);
    try { await action(); }
    catch (e) { setError(String(e)); }
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
      throw new Error("Choose 1–120 seconds of one person's speech.");
    }
    let source: VoiceSampleSource;
    if (kind === "file") {
      if (!file) throw new Error("Choose an audio or video file first.");
      source = { kind: "file", path: file, start_ms: a, end_ms: b };
    } else {
      if (!meetingId) throw new Error("Choose a recording first.");
      source = { kind: "meeting", meeting_id: meetingId, start_ms: a, end_ms: b };
    }
    await cmd.addVoiceSample(selected, source);
    setSamples(await cmd.listVoiceSamples(selected));
    await reload();
  });

  const chooseFile = () => run(async () => {
    const path = await open({ multiple: false, title: "Choose one person's speech", filters: [{ name: "Audio and video", extensions: ["wav", "mp3", "m4a", "aac", "flac", "ogg", "opus", "mp4", "mov", "mkv", "webm"] }] });
    if (typeof path === "string") setFile(path);
  });

  return (
    <div className="mx-auto max-w-[1200px] px-8 py-7">
      <div className="mb-6">
        <h1 className="font-serif text-[30px] tracking-tight">Voices</h1>
        <p className="mt-1 max-w-2xl text-sm text-ink-3">Keep several clear speech samples for each person. Hark suggests a name in future recordings for you to confirm.</p>
        <p className="mt-1 text-[12px] text-ink-3">Voice samples stay in your local library and are excluded from recording exports.</p>
      </div>
      {error && <p role="alert" className="mb-4 rounded-md border border-ember/30 bg-ember-soft px-3 py-2 text-sm text-ember">{error}</p>}
      <div className="grid gap-6 lg:grid-cols-[260px_minmax(0,1fr)]">
        <div>
          <SectionTitle>People</SectionTitle>
          <Card className="overflow-hidden">
            {profiles === null ? <div className="p-4"><Spinner /></div> : profiles.length === 0 ? <p className="p-4 text-sm text-ink-3">Add a person to start a voice profile.</p> : (
              <ul className="divide-y divide-line">
                {profiles.map((p) => <li key={p.id}><button disabled={busy} className={`focus-ring w-full px-4 py-3 text-left ${selected === p.id ? "bg-canvas-3" : "hover:bg-canvas-2"}`} onClick={() => { setSelected(p.id); setError(null); }}>
                  <span className="block text-sm font-medium">{p.name}</span>
                  <span className="text-[12px] text-ink-3">{p.sample_count} sample{p.sample_count === 1 ? "" : "s"}{!p.has_voiceprint && " · no voiceprint yet"}</span>
                </button></li>)}
              </ul>
            )}
          </Card>
          <form className="mt-3 flex gap-2" onSubmit={(e) => { e.preventDefault(); void create(); }}>
            <Input aria-label="New person's name" placeholder="Person's name" maxLength={200} value={newName} disabled={busy} onChange={(e) => setNewName(e.target.value)} />
            <Button type="submit" size="icon" disabled={busy || !newName.trim()} aria-label="Add person"><Plus size={16} /></Button>
          </form>
        </div>
        {profile ? (
          <div>
            <SectionTitle>Voice profile</SectionTitle>
            <Card className="p-4">
              <form className="flex flex-wrap items-center gap-2" onSubmit={(e) => { e.preventDefault(); void run(async () => { await cmd.renameVoiceProfile(profile.id, name.trim()); await reload(); }); }}>
                <Input className="max-w-xs" aria-label="Profile name" maxLength={200} value={name} disabled={busy} onChange={(e) => setName(e.target.value)} />
                <Button type="submit" disabled={busy || !name.trim() || name.trim() === profile.name}>Save name</Button>
                <Button variant="ghost" disabled={busy || !profile.sample_count} onClick={() => void run(async () => { await cmd.recomputeVoiceProfile(profile.id); await reload(); })}><RefreshCw size={14} /> Recompute voice</Button>
                <Button variant="danger" className="ml-auto" disabled={busy} onClick={() => {
                  if (confirm(`Delete ${profile.name}'s voice profile and all its samples?`)) void run(async () => { await cmd.deleteKnownSpeaker(profile.id); setSelected(null); await reload(); });
                }}><Trash2 size={14} /> Delete profile</Button>
              </form>
              <p className="mt-2 text-[12px] text-ink-3">Profile names are used for future suggestions. Edit names in a meeting to change its transcript.</p>
            </Card>

            <div className="mt-6"><SectionTitle>Samples</SectionTitle>
              {loadingSamples ? <Spinner /> : samples.length === 0 ? <p className="text-sm text-ink-3">Add a speech sample to remember this voice.</p> : <ul className="space-y-2">
                {samples.map((s) => <li key={s.id}><Card className="px-4 py-3">
                  <div className="flex items-center gap-3">
                    <AudioLines size={18} className="shrink-0 text-ink-3" />
                    <div className="min-w-0 flex-1">
                      <div className="truncate text-sm font-medium">{s.source_label || "Saved voiceprint"}</div>
                      <div className="text-[12px] text-ink-3">{s.source === "legacy" ? "Existing voice memory" : s.source === "meeting" ? "From a recording" : "Imported file"}{s.start_ms != null && s.end_ms != null && ` · ${fmtDuration(s.start_ms)}–${fmtDuration(s.end_ms)}`}{!s.has_audio && " · voiceprint only"}</div>
                    </div>
                    {s.has_audio && <Button size="sm" variant="ghost" disabled={busy} onClick={() => void run(async () => { setAudio({ id: s.id, url: convertFileSrc(await cmd.voiceSampleAudio(s.id)) }); })}>Listen</Button>}
                    <Button size="iconSm" variant="danger" aria-label={`Delete sample ${s.source_label || s.id}`} disabled={busy} onClick={() => void run(async () => { await cmd.deleteVoiceSample(s.id); if (audio?.id === s.id) setAudio(null); setSamples(await cmd.listVoiceSamples(profile.id)); await reload(); })}><Trash2 size={14} /></Button>
                  </div>
                  {audio?.id === s.id && <audio key={audio.url} controls autoPlay src={audio.url} className="mt-3 h-9 w-full" />}
                </Card></li>)}
              </ul>}
            </div>

            <form className="mt-6" onSubmit={(e) => { e.preventDefault(); void addSample(); }}>
              <SectionTitle>Add speech sample</SectionTitle>
              <Card className="space-y-4 p-4">
                <p className="text-[13px] text-ink-3">Choose 1–120 seconds containing one person. Trim out other voices and long silence. Speaker recognition models must be downloaded in <Link to="/settings" className="underline">Settings</Link>.</p>
                <div className="grid gap-3 sm:grid-cols-2">
                  <label className="space-y-1 text-[12px] text-ink-3">Source<Select aria-label="Sample source" value={kind} disabled={busy} onChange={(e) => setKind(e.target.value as "file" | "meeting")}><option value="file">Audio or video file</option><option value="meeting">Existing recording</option></Select></label>
                  {kind === "file" ? <div className="flex min-w-0 items-end gap-2"><Button disabled={busy} onClick={() => void chooseFile()}><FileAudio size={14} /> Choose file</Button><span className="mb-2 truncate text-[12px] text-ink-3" title={file ?? undefined}>{file?.split(/[\\/]/).pop() ?? "No file chosen"}</span></div> : <label className="space-y-1 text-[12px] text-ink-3">Recording<Select aria-label="Sample recording" value={meetingId} disabled={busy} onChange={(e) => setMeetingId(e.target.value)}><option value="">Choose recording</option>{meetings.map((m) => <option key={m.id} value={m.id}>{m.title} ({fmtDuration(m.duration_ms)})</option>)}</Select></label>}
                </div>
                <div className="flex flex-wrap items-end gap-3">
                  <label className="w-32 space-y-1 text-[12px] text-ink-3">Start (seconds)<Input aria-label="Sample start seconds" type="number" min="0" step="0.001" value={start} disabled={busy} onChange={(e) => setStart(e.target.value)} /></label>
                  <label className="w-32 space-y-1 text-[12px] text-ink-3">End (seconds)<Input aria-label="Sample end seconds" type="number" min="1" step="0.001" value={end} disabled={busy} onChange={(e) => setEnd(e.target.value)} /></label>
                  <Button type="submit" variant="primary" disabled={busy}>{busy ? <Spinner /> : <Plus size={14} />} Add sample</Button>
                </div>
              </Card>
            </form>
          </div>
        ) : <div className="flex min-h-48 items-center justify-center text-sm text-ink-3">Choose a person to manage their voice samples.</div>}
      </div>
    </div>
  );
}
