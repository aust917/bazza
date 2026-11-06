import { useEffect, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/cn";
import { fmtDuration } from "@/lib/format";

interface OperationProgressProps {
  label: string;
  accessibleLabel?: string;
  id?: string;
  announceStage?: boolean;
  value?: number;
  detail?: ReactNode;
  className?: string;
  startedAt?: number;
  finishedAt?: number;
}

export function OperationProgress({ label, accessibleLabel, id, announceStage, value, detail, className, startedAt, finishedAt }: OperationProgressProps) {
  const { t } = useTranslation("main");
  const [now, setNow] = useState(() => performance.now());
  useEffect(() => {
    if (startedAt === undefined || finishedAt !== undefined) return;
    const timer = setInterval(() => setNow(performance.now()), 1000);
    return () => clearInterval(timer);
  }, [startedAt, finishedAt]);

  const elapsed = startedAt === undefined ? undefined : fmtDuration(Math.max(0, (finishedAt ?? now) - startedAt));
  const elapsedLabel = elapsed === undefined ? undefined : t("operationProgress.elapsed", { time: elapsed });
  const percent = value === undefined || !Number.isFinite(value)
    ? undefined
    : Math.min(value >= 1 ? 100 : 99, Math.max(0, Math.round(value * 100)));

  return (
    <div id={id} className={cn("min-w-0 space-y-1 text-xs text-ink-2", className)}>
      <div className="flex items-baseline justify-between gap-2">
        <span role={announceStage ? "status" : undefined} className="min-w-0 truncate" title={label}>{label}</span>
        <span className="flex shrink-0 gap-2 font-mono tabular-nums">
          {percent !== undefined && <span>{percent}%</span>}
          {elapsed !== undefined && <time aria-label={elapsedLabel} title={elapsedLabel}>{elapsed}</time>}
        </span>
      </div>
      <div role="progressbar" aria-label={accessibleLabel ?? label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent}
        className="h-1 overflow-hidden rounded-full bg-canvas-3">
        <div aria-hidden="true" className={cn("h-full rounded-full bg-ember",
          percent === undefined ? "w-1/3 motion-safe:animate-pulse" : "motion-safe:transition-[width]")}
          style={percent === undefined ? undefined : { width: `${percent}%` }} />
      </div>
      {detail != null && <div className="text-[11px] text-ink-2">{detail}</div>}
    </div>
  );
}
