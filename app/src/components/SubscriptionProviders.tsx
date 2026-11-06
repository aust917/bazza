import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { cmd, type SubscriptionAvailability, type SummaryAgent } from "@/lib/ipc";
import { Button } from "./ui";

type Providers = SubscriptionAvailability[] | "checking" | "check_failed";

const agents = [
  { id: "codex", name: "Codex" },
  { id: "claude", name: "Claude Code" },
] as const;

function statusOf(providers: Providers, agent: SummaryAgent) {
  return Array.isArray(providers)
    ? providers.find((provider) => provider.agent === agent)?.status ?? "check_failed"
    : providers;
}

export function useSubscriptionProviders() {
  const [providers, setProviders] = useState<Providers>("checking");
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let active = true;
    void cmd.subscriptionAgents().then(
      (result) => { if (active) setProviders(result); },
      () => { if (active) setProviders("check_failed"); },
    );
    return () => { active = false; };
  }, [revision]);
  return {
    providers,
    ready: (agent: SummaryAgent) => statusOf(providers, agent) === "ready",
    refresh: () => {
      setProviders("checking");
      setRevision((value) => value + 1);
    },
  };
}

export function SubscriptionOptions({ providers }: { providers: Providers }) {
  const { t } = useTranslation("meeting");
  return agents.map(({ id, name }) => {
    const status = statusOf(providers, id);
    return <option key={id} value={id} disabled={status !== "ready"}>
      {name} · {t(`subscription.${status}`)}
    </option>;
  });
}

export function SubscriptionStatuses() {
  const { t } = useTranslation("meeting");
  const { providers, refresh } = useSubscriptionProviders();
  return (
    <div className="mt-2 space-y-2">
      <ul className="space-y-1 text-[13px] text-ink-2" aria-live="polite">
        {agents.map(({ id, name }) => <li key={id}>{name} · {t(`subscription.${statusOf(providers, id)}`)}</li>)}
      </ul>
      <Button type="button" variant="outline" size="sm" disabled={providers === "checking"} onClick={refresh}>
        {t("subscription.refresh")}
      </Button>
    </div>
  );
}
