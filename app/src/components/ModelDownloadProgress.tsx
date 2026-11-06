import { useTranslation } from "react-i18next";
import { OperationProgress } from "@/components/ui/OperationProgress";
import { fmtBytes } from "@/lib/format";
import { displayError } from "@/lib/i18n";

export function ModelDownloadProgress({ modelName, done, total, startedAt }: {
  modelName: string;
  done?: number;
  total?: number;
  startedAt?: number;
}) {
  const { t } = useTranslation("main");
  const knownSize = total != null && Number.isFinite(total) && total > 0 && done != null && Number.isFinite(done);
  const received = knownSize ? Math.max(0, Math.min(done, total)) : undefined;
  const value = received == null || total == null ? undefined : received / total;
  return (
    <OperationProgress
      label={t(value == null ? "modelDownload.preparing" : value >= 1 ? "modelDownload.finishing" : "modelDownload.downloading")}
      accessibleLabel={t("modelDownload.progressLabel", { model: modelName })}
      value={value != null && value < 1 ? value : undefined}
      detail={received != null && total != null ? t("modelDownload.bytes", { done: fmtBytes(received), total: fmtBytes(total) }) : undefined}
      startedAt={startedAt}
      className="mt-1.5"
    />
  );
}

export function ModelDownloadFailure({ error }: { error: unknown }) {
  const { t } = useTranslation("main");
  if (error === "cancelled") {
    return <p role="status" className="mt-1 text-[12px] text-ink-3">{t("modelDownload.canceled")}</p>;
  }
  return (
    <p role="alert" className="mt-1 break-words text-[12px] text-ember">
      {t("modelDownload.failed")}{error != null && ` ${displayError(error)}`}
    </p>
  );
}
