import { useCallback, useEffect, useRef, useState } from "react";
import { cmd, subscribe, type ModelRow } from "./ipc";

type ModelDownload =
  | { status: "downloading"; done?: number; total?: number; startedAt?: number }
  | { status: "failed"; error: unknown };

const preparing: ModelDownload = { status: "downloading" };

export function useModelDownloads(onSettled?: () => void) {
  const [models, setModels] = useState<ModelRow[]>([]);
  const [downloads, setDownloads] = useState<Partial<Record<string, ModelDownload>>>({});
  const requests = useRef(new Map<string, symbol>());
  const listRevision = useRef(0);
  const mounted = useRef(false);

  const reloadModels = useCallback(async () => {
    const revision = ++listRevision.current;
    const next = await cmd.listModels();
    if (mounted.current && revision === listRevision.current) setModels(next);
  }, []);

  const finish = useCallback((id: string, result: { status: "done" } | { status: "failed"; error: unknown }) => {
    setDownloads((previous) => ({ ...previous, [id]: result.status === "failed" ? result : undefined }));
    setModels((previous) => previous.map((model) => model.spec.id === id
      ? { ...model, present: model.present || result.status === "done", downloading: false }
      : model));
    void reloadModels().catch(() => {});
    onSettled?.();
  }, [onSettled, reloadModels]);

  useEffect(() => {
    mounted.current = true;
    const unsubscribe = subscribe("model_progress", (event) => {
      const { id, status, done, total, error } = event;
      if (status === "downloading") {
        setDownloads((previous) => {
          const prior = previous[id];
          const startedAt = prior?.status === "downloading" ? prior.startedAt : undefined;
          return { ...previous, [id]: { status, done, total, startedAt } };
        });
      } else {
        // Native clears its download lock before sending a terminal event.
        requests.current.delete(id);
        finish(id, status === "done" ? { status } : { status, error });
      }
    });
    void reloadModels().catch(() => {});
    return () => {
      mounted.current = false;
      unsubscribe();
    };
  }, [finish, reloadModels]);

  const getDownload = (model: ModelRow) => {
    if (model.present) return undefined;
    return downloads[model.spec.id] ?? (model.downloading ? preparing : undefined);
  };

  const download = async (model: ModelRow) => {
    const id = model.spec.id;
    if (model.present || getDownload(model)?.status === "downloading" || requests.current.has(id)) return;
    const request = Symbol(id);
    requests.current.set(id, request);
    const stillCurrent = () => mounted.current && requests.current.get(id) === request;
    const startedAt = performance.now();
    setDownloads((previous) => ({ ...previous, [id]: { status: "downloading", startedAt } }));
    try {
      await cmd.downloadModel(id);
      if (stillCurrent()) finish(id, { status: "done" });
    } catch (error) {
      if (stillCurrent()) {
        if (error && typeof error === "object" && "code" in error && error.code === "native.modelDownloading") {
          setDownloads((previous) => {
            const current = previous[id];
            return current?.status === "downloading" && current.done === undefined
              ? { ...previous, [id]: undefined }
              : previous;
          });
          void reloadModels().catch(() => {});
        } else {
          finish(id, { status: "failed", error });
        }
      }
    } finally {
      if (requests.current.get(id) === request) requests.current.delete(id);
    }
  };

  return { models, reloadModels, getDownload, download };
}
