import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router-dom";
import { ChatPanel } from "@/components/ChatPanel";

/** tr("nav.ask") across every meeting. Citations open the meeting at that time. */
export function AskPage() {
  const { t: tr } = useTranslation("main");
  const navigate = useNavigate();
  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col px-8 py-8">
      <header className="mb-4">
        <h1 className="font-sans font-semibold text-[34px] leading-none tracking-tight">{tr("nav.ask")}</h1>
        <p className="mt-1.5 text-[13px] text-ink-3">{tr("ask.description")}</p>
      </header>
      <ChatPanel
        scopeKind="all"
        onSeek={(ms, meetingId) => {
          if (meetingId) navigate(`/meeting/${meetingId}?t=${ms}`);
        }}
        placeholder={tr("ask.placeholder")}
        className="min-h-0 flex-1"
      />
    </div>
  );
}
