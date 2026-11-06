import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, SectionTitle } from "@/components/ui";
import { displayError } from "@/lib/i18n";
import { cmd, type AttendeeTrack } from "@/lib/ipc";

export function AttendeeTracks({ meetingId }: { meetingId: string }) {
  const { t } = useTranslation("meeting");
  const [tracks, setTracks] = useState<AttendeeTrack[]>([]);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    let cancelled = false;
    void cmd.attendeeTracks(meetingId).then((value) => {
      if (!cancelled) setTracks(value);
    }).catch((err) => { if (!cancelled) setError(err); });
    return () => { cancelled = true; };
  }, [meetingId]);

  if (tracks.length === 0 && error === null) return null;

  const open = async (trackId: number) => {
    setError(null);
    try { await cmd.attendeeOpenTrack(meetingId, trackId); }
    catch (err) { setError(err); }
  };

  return (
    <section className="mt-6">
      <SectionTitle>{t("attendeeTracks.title")}</SectionTitle>
      {tracks.length > 0 && <ul className="divide-y divide-line rounded-lg border border-line bg-canvas-2 px-3">
        {tracks.map(({ id, name }) => {
          const label = name ?? t("attendeeTracks.unnamed", { id });
          return (
            <li key={id} className="flex items-center justify-between gap-2 py-1">
              <span className="min-w-0 truncate text-[13px] text-ink" title={label}>{label}</span>
              <Button variant="ghost" size="sm" className="shrink-0" aria-label={t("attendeeTracks.openNamed", { name: label })} onClick={() => void open(id)}>{t("attendeeTracks.open")}</Button>
            </li>
          );
        })}
      </ul>}
      {error !== null && <p role="alert" className="mt-1 text-[12px] text-ember">{displayError(error)}</p>}
    </section>
  );
}
