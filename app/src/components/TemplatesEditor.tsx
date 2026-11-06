import { useTranslation } from "react-i18next";
import { Copy, Plus, Trash2 } from "lucide-react";
import { useEffect, useState } from "react";
import { displayError } from "@/lib/i18n";
import { templateName, templateDescription } from "./templateLabels";
import { cn } from "@/lib/cn";
import { cmd, type Template } from "@/lib/ipc";
import { Button, Input } from "./ui";

const PLACEHOLDERS = ["{{title}}", "{{date}}", "{{duration}}", "{{participants}}", "{{transcript}}", "{{highlights}}"];
type TemplateFeedback = { kind: "saved" } | { kind: "error"; error: unknown };

/** List + editor for summary templates. Built-ins can be edited or duplicated, not deleted. */
export function TemplatesEditor({ defaultId, onDefaultChange }: { defaultId: string; onDefaultChange: (id: string) => void }) {
  const { t: tr } = useTranslation("meeting");
  const [templates, setTemplates] = useState<Template[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [draft, setDraft] = useState<Template | null>(null);
  const [status, setStatus] = useState<TemplateFeedback | null>(null);

  const reload = async () => {
    const t = await cmd.listTemplates();
    setTemplates(t);
    if (!selected && t[0]) setSelected(t[0].id);
  };
  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  useEffect(() => {
    setDraft(templates.find((t) => t.id === selected) ?? null);
  }, [selected, templates]);

  const save = async () => {
    if (!draft) return;
    try {
      const saved = await cmd.saveTemplate(draft);
      setStatus({ kind: "saved" });
      await reload();
      setSelected(saved.id);
    } catch (e) {
      setStatus({ kind: "error", error: e });
    }
    setTimeout(() => setStatus(null), 1500);
  };

  const duplicate = async () => {
    if (!draft) return;
    const copy = await cmd.saveTemplate({ ...draft, id: "", name: tr("templates.copyName", { name: draft.name }), builtin: false });
    await reload();
    setSelected(copy.id);
  };

  const create = async () => {
    const t = await cmd.saveTemplate({ id: "", name: tr("templates.new"), description: "", builtin: false, body: "Write notes for {{title}} ({{date}}). Participants: {{participants}}.\n\nProduce these sections with \"## \" headings:\n\n## Summary\n\n## Action items\n\n## Key moments\nEach bullet starts with the timestamp in square brackets.\n\nTranscript:\n{{transcript}}" });
    await reload();
    setSelected(t.id);
  };

  const remove = async () => {
    if (!draft || draft.builtin) return;
    if (!confirm(tr("templates.deleteConfirm", { name: draft.name }))) return;
    await cmd.deleteTemplate(draft.id);
    setSelected(null);
    await reload();
  };

  return (
    <div className="grid grid-cols-[200px_minmax(0,1fr)] gap-4">
      <div className="flex flex-col gap-1">
        {templates.map((t) => (
          <button
            key={t.id}
            onClick={() => setSelected(t.id)}
            className={cn("focus-ring flex items-center justify-between rounded-md px-2.5 py-1.5 text-left text-[13px]", t.id === selected ? "bg-canvas-3 text-ink" : "text-ink-2 hover:bg-canvas-2")}
          >
            <span className="truncate" title={templateDescription(t, tr)}>{templateName(t, tr)}</span>
            {t.id === defaultId && <span className="ml-2 rounded bg-moss-soft px-1 text-[9px] font-semibold tracking-wide text-moss uppercase">{tr("templates.default")}</span>}
          </button>
        ))}
        <Button variant="ghost" size="sm" className="mt-1 justify-start" onClick={() => void create()}>
          <Plus size={14} /> {tr("templates.new")}
        </Button>
      </div>

      {draft ? (
        <div className="flex flex-col gap-3">
          <div className="grid grid-cols-2 gap-2">
            <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder={tr("templates.name")} />
            <Input value={draft.description} onChange={(e) => setDraft({ ...draft, description: e.target.value })} placeholder={tr("templates.description")} />
          </div>
          <textarea
            aria-label={tr("templates.body")}
            value={draft.body}
            onChange={(e) => setDraft({ ...draft, body: e.target.value })}
            spellCheck={false}
            className="focus-ring min-h-[280px] w-full resize-y rounded-md border border-line-2 bg-canvas p-3 font-mono text-[12px] leading-5 select-text"
          />
          <div className="flex flex-wrap items-center gap-1 text-[11px] text-ink-3">
            {tr("templates.placeholders")}
            {PLACEHOLDERS.map((p) => (
              <code key={p} className="rounded bg-canvas-3 px-1.5 py-0.5 font-mono">{p}</code>
            ))}
          </div>
          <div className="flex items-center gap-2">
            <Button variant="primary" size="sm" onClick={() => void save()}>{tr("actions.save")}</Button>
            <Button variant="outline" size="sm" onClick={() => void duplicate()}><Copy size={13} /> {tr("templates.duplicate")}</Button>
            {draft.id !== defaultId && <Button variant="ghost" size="sm" onClick={() => onDefaultChange(draft.id)}>{tr("templates.useDefault")}</Button>}
            {!draft.builtin && <Button variant="danger" size="sm" onClick={() => void remove()}><Trash2 size={13} /> {tr("actions.delete")}</Button>}
            <span role="status" className="ml-auto text-[12px] text-moss">{status?.kind === "saved" ? tr("templates.saved") : status?.kind === "error" ? displayError(status.error) : ""}</span>
          </div>
        </div>
      ) : (
        <p className="text-[13px] text-ink-3">{tr("templates.select")}</p>
      )}
    </div>
  );
}
