import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card, Input, SectionTitle } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { displayError } from "@/lib/i18n";
import { cmd, subscribe, type AttendeeSettings as ConnectionSettings, type AttendeeSettingsStatus, type AttendeeSetupProgress } from "@/lib/ipc";

const fields = [
  { name: "apiUrl", placeholder: "http://127.0.0.1:18760", type: "url" },
  { name: "callbackUrl", placeholder: "wss://bazza-attendee.local:18761/audio", type: "url" },
  { name: "bindAddress", placeholder: "127.0.0.1:18761", type: "text" },
  { name: "certificatePath", placeholder: "certificate.pem", type: "text" },
  { name: "privateKeyPath", placeholder: "private-key.pem", type: "text" },
] as const;

function connectionDraft(status?: AttendeeSettingsStatus): ConnectionSettings {
  return {
    apiUrl: status?.apiUrl ?? "",
    callbackUrl: status?.callbackUrl ?? "",
    bindAddress: status?.bindAddress ?? "",
    certificatePath: status?.certificatePath ?? "",
    privateKeyPath: status?.privateKeyPath ?? "",
  };
}

type SetupProgress = {
  phase: AttendeeSetupProgress["phase"] | "complete";
  startedAt: number;
  finishedAt?: number;
};

function AttendeeSetupIndicator({ phase, startedAt, finishedAt }: SetupProgress) {
  const { t } = useTranslation("main");
  return (
    <OperationProgress
      label={phase === "complete" ? t("attendee.managedReady") : t(`attendee.progress.${phase}`)}
      accessibleLabel={t("attendee.progressLabel")}
      value={phase === "complete" ? 1 : undefined}
      startedAt={startedAt}
      finishedAt={finishedAt}
      announceStage
      className={phase === "complete" ? "font-medium text-moss" : undefined}
    />
  );
}

export function AttendeeSettings() {
  const { t } = useTranslation("main");
  const [status, setStatus] = useState<AttendeeSettingsStatus | null>(null);
  const [draft, setDraft] = useState(connectionDraft);
  const [apiKey, setApiKey] = useState("");
  const [advanced, setAdvanced] = useState(false);
  const [busy, setBusy] = useState(false);
  const [setupProgress, setSetupProgress] = useState<SetupProgress | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [revision, setRevision] = useState(0);
  const inFlight = useRef<"manual" | "setup" | null>(null);
  const requiresKey = !status?.configured || status.managed || draft.apiUrl.trim().replace(/\/+$/, "") !== status.apiUrl;

  const apply = (value: AttendeeSettingsStatus) => {
    setStatus(value);
    setDraft(connectionDraft(value.managed ? undefined : value));
    setApiKey("");
  };

  useEffect(() => {
    let cancelled = false;
    void cmd.attendeeStatus().then((value) => {
      if (!cancelled) apply(value);
    }).catch((err) => { if (!cancelled) setError(err); });
    return () => { cancelled = true; };
  }, [revision]);

  useEffect(() => subscribe("attendee_setup_progress", ({ phase }) => {
    setSetupProgress((progress) => {
      if (!progress || progress.phase === "complete") return progress;
      return { ...progress, phase };
    });
  }), []);

  const update = async (command: () => Promise<AttendeeSettingsStatus>, operation: "manual" | "setup" = "manual") => {
    if (inFlight.current) return;
    inFlight.current = operation;
    setBusy(true);
    setSetupProgress(operation === "setup" ? { phase: "preparing", startedAt: performance.now() } : null);
    setError(null);
    try {
      const next = await command();
      apply(next);
      if (operation === "setup") {
        if (next.configured && next.managed) {
          const finishedAt = performance.now();
          setSetupProgress((progress) => progress && { ...progress, phase: "complete", finishedAt });
          setAdvanced(false);
        } else {
          setSetupProgress(null);
        }
      }
    }
    catch (err) {
      setError(err);
      setSetupProgress(null);
    }
    finally {
      inFlight.current = null;
      setBusy(false);
    }
  };

  return (
    <section id="google-meet" tabIndex={-1} aria-label={t("attendee.title")} className="mb-10 scroll-mt-4">
      <SectionTitle>{t("attendee.title")}</SectionTitle>
      <Card className="space-y-3 p-5">
        <p className="text-[13px] text-ink-2">{t("attendee.description")}</p>
        {!status && error === null && <p role="status" className="text-[12px] text-ink-3">{t("attendee.loading")}</p>}
        {status && <>
          {status.configured && setupProgress === null && <p role="status" className="text-[13px] font-medium text-moss">{t(status.managed ? "attendee.managedReady" : "attendee.configured")}</p>}
          {status.setupAvailable !== false && (!status.configured || status.managed) && <>
            {!status.configured && <p className="text-[12px] text-ink-3">{t("attendee.setupHint")}</p>}
            <Button type="button" variant={status.configured ? "outline" : "ember"} size="md" disabled={busy} onClick={() => void update(cmd.attendeeSetup, "setup")}>
              {t(status.configured ? "attendee.repair" : "attendee.setup")}
            </Button>
            {setupProgress !== null && <AttendeeSetupIndicator {...setupProgress} />}
          </>}
          {!status.configured && status.setupAvailable === false && <p className="text-[12px] text-ink-3">{t("attendee.setupUnsupported")}</p>}
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="ghost" size="sm" disabled={busy} aria-expanded={advanced} onClick={() => setAdvanced(!advanced)}>{t("attendee.advanced")}</Button>
            {(status.configured || status.apiUrl) && <Button type="button" variant="ghost" size="sm" disabled={busy} onClick={() => void update(cmd.attendeeDisconnect)}>{t("attendee.disconnect")}</Button>}
          </div>
          {advanced && <form className="space-y-3 border-t border-line pt-3" onSubmit={(event) => {
            event.preventDefault();
            void update(() => cmd.attendeeConfigure({
              apiUrl: draft.apiUrl.trim(), callbackUrl: draft.callbackUrl.trim(), bindAddress: draft.bindAddress.trim(),
              certificatePath: draft.certificatePath.trim(), privateKeyPath: draft.privateKeyPath.trim(),
            }, apiKey.trim() || null));
          }}>
            <div className="grid gap-3 sm:grid-cols-2">
              {fields.map(({ name, placeholder, type }) => (
                <label key={name} className="block text-[12px] text-ink-2">
                  <span className="mb-1 block font-medium">{t(`attendee.fields.${name}`)}</span>
                  <Input type={type} autoComplete="off" spellCheck={false} placeholder={placeholder} value={draft[name]} disabled={busy} required onChange={(event) => setDraft({ ...draft, [name]: event.target.value })} />
                </label>
              ))}
              <label className="block text-[12px] text-ink-2">
                <span className="mb-1 block font-medium">{t("attendee.apiKey")}</span>
                <Input type="password" autoComplete="off" spellCheck={false} placeholder={t(requiresKey ? "attendee.enterKey" : "attendee.keepKey")} value={apiKey} disabled={busy} required={requiresKey} onChange={(event) => setApiKey(event.target.value)} />
              </label>
            </div>
            <p className="text-[12px] text-ink-3">{t("attendee.networkHint")}</p>
            <p className="text-[12px] text-ink-3">{t("attendee.tlsHint")}</p>
            <Button type="submit" variant="outline" size="md" disabled={busy || fields.some(({ name }) => !draft[name].trim()) || (requiresKey && !apiKey.trim())}>{t("attendee.save")}</Button>
          </form>}
          <p className="text-[12px] text-ink-3">{t("attendee.filesHint")}</p>
        </>}
        {error !== null && <div className="space-y-2">
          <p role="alert" className="text-[12px] text-ember">{displayError(error)}</p>
          {!status && <Button type="button" variant="outline" size="sm" onClick={() => { setError(null); setRevision(revision + 1); }}>{t("attendee.retry")}</Button>}
        </div>}
      </Card>
    </section>
  );
}
