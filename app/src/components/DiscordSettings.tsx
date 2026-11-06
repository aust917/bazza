import { openUrl } from "@tauri-apps/plugin-opener";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { DiscordChannelPicker, discordChannelUrl } from "@/components/DiscordChannelPicker";
import { Button, Card, Input, SectionTitle } from "@/components/ui";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { displayError } from "@/lib/i18n";
import { cmd, type DiscordChannel, type DiscordSettingsStatus } from "@/lib/ipc";
import { isMock } from "@/lib/mock";

const developerPortal = "https://discord.com/developers/applications";
const setupSteps = ["create", "token", "invite", "channel"] as const;
type SetupStep = typeof setupSteps[number] | "ready";
type ValidationStage = "token" | "channel" | "invite";

async function openDiscordPage(url: string) {
  if (isMock) window.open(url, "_blank", "noopener,noreferrer");
  else await openUrl(url);
}

export function DiscordSettings() {
  const { t } = useTranslation("main");
  const [status, setStatus] = useState<DiscordSettingsStatus | null>(null);
  const [token, setToken] = useState("");
  const [link, setLink] = useState("");
  const [choice, setChoice] = useState<DiscordChannel | null>(null);
  const [stepOverride, setStepOverride] = useState<SetupStep | null>(null);
  const [pickerRevision, setPickerRevision] = useState(0);
  const [busy, setBusy] = useState(false);
  const [validation, setValidation] = useState<{ stage: ValidationStage; startedAt: number } | null>(null);
  const inFlight = useRef(false);
  const [error, setError] = useState<unknown>(null);
  const [copied, setCopied] = useState<"invite" | "assistant" | null>(null);
  const step = stepOverride ?? (status?.channel ? "ready" : status?.bot ? "invite" : "create");
  const stepIndex = step === "ready" ? setupSteps.length : setupSteps.indexOf(step);

  useEffect(() => {
    let cancelled = false;
    void cmd.discordStatus().then((value) => {
      if (!cancelled) setStatus(value);
    }, (failure) => { if (!cancelled) setError(failure); });
    return () => { cancelled = true; };
  }, []);

  const run = async (action: () => Promise<void>, stage?: ValidationStage) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setError(null);
    setCopied(null);
    setBusy(true);
    if (stage) setValidation({ stage, startedAt: performance.now() });
    try { await action(); }
    catch (failure) { setError(failure); }
    finally {
      inFlight.current = false;
      setBusy(false);
      setValidation(null);
    }
  };

  const goTo = (next: SetupStep | null) => {
    setError(null);
    setCopied(null);
    setToken("");
    setStepOverride(next);
  };
  const openPortal = () => void run(cmd.discordOpenDeveloperPortal);
  const copyAssistantGuide = () => void run(async () => {
    await navigator.clipboard.writeText(t("discord.wizard.assistantGuide", { developerPortal }));
    setCopied("assistant");
  });
  const connect = () => void run(async () => {
    const next = await cmd.discordSaveToken(token.trim());
    setStatus(next);
    setToken("");
    setChoice(next.channel);
    setStepOverride(null);
  }, "token");
  const saveChannel = (input: string) => void run(async () => {
    const saved = await cmd.discordSetChannel(input);
    setStatus((current) => current ? { ...current, channel: saved } : current);
    setChoice(saved);
    setStepOverride(null);
    setLink("");
  }, "channel");
  const invite = (copy: boolean) => void run(async () => {
    const url = await cmd.discordInviteUrl(null);
    if (copy) {
      await navigator.clipboard.writeText(url);
      setCopied("invite");
    } else await openDiscordPage(url);
  }, "invite");
  const chooseChannel = () => {
    setChoice(status?.channel ?? null);
    goTo("channel");
  };
  const disconnect = () => void run(async () => {
    setStatus(await cmd.discordDisconnect());
    setLink("");
    setChoice(null);
    goTo(null);
  });

  return (
    <section id="discord" tabIndex={-1} aria-label={t("discord.title")} className="mb-10 scroll-mt-4">
      <SectionTitle>{t("discord.title")}</SectionTitle>
      <Card className="space-y-3 px-5 py-4">
        <p className="text-[13px] text-ink-2">{t("discord.description")}</p>
        {!status && error == null && <p role="status" className="text-[12px] text-ink-3">{t("discord.loading")}</p>}
        {status && <>
          <ol aria-label={t("discord.wizard.steps")} className="grid gap-2 border-y border-line py-3 text-[12px] sm:grid-cols-2">
            {setupSteps.map((id, index) => (
              <li key={id} aria-current={step === id ? "step" : undefined} className={step === id ? "font-semibold text-ink" : index < stepIndex ? "text-moss" : "text-ink-3"}>
                {index + 1}. {t(`discord.wizard.${id}`)}
              </li>
            ))}
          </ol>
          {status.bot && <p role="status" className="text-[13px] font-medium text-moss">{t("discord.connected", { name: status.bot.name })}</p>}
          {step === "create" && <>
            <p className="text-[13px] text-ink-2">{t("discord.wizard.createHint")}</p>
            {status.bot && <p className="text-[12px] text-ink-3">{t("discord.wizard.replaceHint")}</p>}
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" disabled={busy} onClick={openPortal}>{t("discord.openPortal")}</Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo("token")}>{t("discord.wizard.continueToToken")}</Button>
              {status.bot && <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo(null)}>{t("discord.wizard.cancel")}</Button>}
            </div>
          </>}
          {step === "token" && <>
            <p className="text-[13px] text-ink-2">{t("discord.tokenHint")}</p>
            <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); if (!busy && token.trim()) connect(); }}>
              <label className="min-w-[190px] flex-1 text-[12px] text-ink-2">
                <span className="mb-1 block font-medium">{t("discord.token")}</span>
                <Input type="password" autoComplete="off" autoCapitalize="none" autoCorrect="off" spellCheck={false} value={token} disabled={busy} onChange={(event) => setToken(event.target.value)} placeholder={t("discord.tokenPlaceholder")} />
              </label>
              <Button type="submit" variant="outline" size="md" disabled={busy || !token.trim()}>{t("discord.saveBot")}</Button>
            </form>
            <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo("create")}>{t("discord.wizard.back")}</Button>
            {status.bot && <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo(null)}>{t("discord.wizard.cancel")}</Button>}
          </>}
          {step === "invite" && <>
            <p className="text-[13px] text-ink-2">{t("discord.inviteHint")}</p>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" disabled={busy} onClick={() => invite(false)}>{t("discord.invite")}</Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => invite(true)}>{t("discord.copyInvite")}</Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={chooseChannel}>{t("discord.wizard.continueToChannel")}</Button>
              {status.channel && <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo(null)}>{t("discord.wizard.cancel")}</Button>}
            </div>
            {copied === "invite" && <p role="status" className="text-[12px] text-moss">{t("discord.copied")}</p>}
          </>}
          {step === "channel" && <>
            <p className="text-[13px] text-ink-2">{t("discord.wizard.channelHint")}</p>
            <DiscordChannelPicker key={pickerRevision} channel={choice} onChange={setChoice} disabled={busy} />
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" disabled={busy || !choice} onClick={() => { if (choice) saveChannel(discordChannelUrl(choice)); }}>{t("discord.saveChannel")}</Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => { setPickerRevision((value) => value + 1); setError(null); }}>{t("discord.wizard.refresh")}</Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo("invite")}>{t("discord.wizard.backToInvite")}</Button>
              {status.channel && <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo(null)}>{t("discord.wizard.cancel")}</Button>}
            </div>
            <form className="flex flex-wrap items-end gap-2" onSubmit={(event) => { event.preventDefault(); if (!busy && link.trim()) saveChannel(link.trim()); }}>
              <label className="min-w-[200px] flex-1 text-[12px] text-ink-2">
                <span className="mb-1 block font-medium">{t("discord.channelLink")}</span>
                <Input type="url" placeholder="https://discord.com/channels/…" value={link} disabled={busy} onChange={(event) => setLink(event.target.value)} />
              </label>
              <Button type="submit" variant="outline" size="md" disabled={busy || !link.trim()}>{t("discord.useLink")}</Button>
            </form>
          </>}
          {step === "ready" && status.channel && <>
            <p role="status" className="text-[13px] text-moss">{t("discord.preferred", { server: status.channel.guild_name, channel: status.channel.channel_name })}</p>
            <p className="text-[13px] text-ink-2">{t("discord.wizard.readyHint")}</p>
            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" disabled={busy} onClick={chooseChannel}>{t("discord.wizard.changeChannel")}</Button>
              <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo("invite")}>{t("discord.invite")}</Button>
            </div>
          </>}
          {status.bot && (step === "invite" || step === "ready") && <div className="flex flex-wrap gap-2 border-t border-line pt-2">
            <Button variant="ghost" size="sm" disabled={busy} onClick={() => goTo("create")}>{t("discord.wizard.replaceBot")}</Button>
            <Button variant="ghost" size="sm" disabled={busy} onClick={disconnect}>{t("discord.disconnect")}</Button>
          </div>}
        </>}
        {validation && (
          <OperationProgress label={t(`discord.progress.${validation.stage}`)} accessibleLabel={t("discord.progress.label")}
            startedAt={validation.startedAt} announceStage />
        )}
        {error != null && <div className="flex items-center gap-2">
          <p role="alert" className="text-[12px] text-ember">{displayError(error)}</p>
          {!status && <Button type="button" variant="outline" size="sm" disabled={busy} onClick={() => void run(async () => setStatus(await cmd.discordStatus()))}>{t("discord.retry")}</Button>}
        </div>}
        {status && <div className="rounded-md bg-canvas-2 p-3 text-[13px] text-ink-2">
          <p className="font-medium text-ink">{t("discord.wizard.assistant")}</p>
          <p className="mt-1">{t("discord.wizard.assistantHint")}</p>
          <Button className="mt-2" variant="outline" size="sm" disabled={busy} onClick={copyAssistantGuide}>{t("discord.wizard.copyAssistant")}</Button>
          {copied === "assistant" && <p role="status" className="mt-2 text-[12px] text-moss">{t("discord.wizard.assistantCopied")}</p>}
        </div>}
      </Card>
    </section>
  );
}
