import { AudioLines } from "lucide-react";
import { useTranslation } from "react-i18next";
import { useBotRecordingStatus } from "@/lib/useBotRecordingStatus";

export function BotRecordingStatus({ meetingId }: { meetingId: string }) {
  const { t } = useTranslation(["meeting", "recording"]);
  const { attendeePhase, discordState } = useBotRecordingStatus(meetingId);
  if (!attendeePhase && !discordState) return null;

  return (
    <section className="mt-3 space-y-1 border-t border-line pt-3">
      <h2 className="flex items-center gap-2 text-[13px] font-semibold text-ink">
        <AudioLines size={14} aria-hidden="true" /> {t("botTranscript.liveTracks")}
      </h2>
      {attendeePhase && <p role="status" className={`text-[13px] ${attendeePhase === "failed" ? "text-ember" : "text-ink-2"}`}>
        {t(`recording:attendeeStatus.${attendeePhase}`)}
      </p>}
      {discordState && <p role="status" className={`text-[13px] ${discordState === "unavailable" ? "text-ember" : "text-ink-2"}`}>
        {discordState === "unavailable" ? t("botTranscript.discordUnavailable") : t(`recording:discordStatus.${discordState}`)}
      </p>}
    </section>
  );
}
