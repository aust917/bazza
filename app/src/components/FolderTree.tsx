import { useTranslation } from "react-i18next";
import { FolderOpen, FolderPlus, Inbox, Layers, Pencil, Trash2 } from "lucide-react";
import { useRef, useState } from "react";
import { cn } from "@/lib/cn";
import { displayError } from "@/lib/i18n";
import { cmd, type Folder, type ServiceFolderMutation, type Tag } from "@/lib/ipc";

export type Selection = { kind: "all" } | { kind: "unfiled" } | { kind: "folder"; id: string } | { kind: "tag"; id: string };

interface FolderTreeProps {
  folders: Folder[];
  tags: Tag[];
  selection: Selection;
  onSelect: (s: Selection) => void;
  onChange: () => void;
  onError: (error: string | null) => void;
  serviceDesktop: boolean;
  /** Folder under the drag pointer. An empty ID represents Unfiled. */
  dragTarget?: string;
}

/** Sidebar-style folder + tag navigation for the Library. */
export function FolderTree({ folders, tags, selection, onSelect, onChange, onError, serviceDesktop, dragTarget }: FolderTreeProps) {
  const { t: tr } = useTranslation("main");
  const [editing, setEditing] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const pending = useRef(false);
  const mutation = useRef<{ payload: string; key: string } | null>(null);

  const children = (parent: string | null) => folders.filter((f) => f.parent_id === parent);

  const change = async (operation: () => Promise<unknown>) => {
    if (pending.current) return false;
    pending.current = true;
    onError(null);
    try {
      await operation();
      return true;
    } catch (error) {
      onError(displayError(error));
      return false;
    } finally {
      pending.current = false;
      onChange();
    }
  };

  const mutate = async (operation: ServiceFolderMutation) => {
    const payload = JSON.stringify(operation);
    if (mutation.current?.payload !== payload) mutation.current = { payload, key: crypto.randomUUID() };
    await cmd.mutateServiceLibraryOrganization({ ...operation, idempotency_key: mutation.current.key });
    mutation.current = null;
  };

  const addFolder = async (parent: string | null) => {
    const name = prompt(parent ? tr("folders.subfolderName") : tr("folders.folderName"));
    if (!name?.trim()) return;
    await change(() => serviceDesktop
      ? mutate({ op: "folder_create", name, parent_id: parent })
      : cmd.createFolder(name, parent));
  };

  const rename = async (f: Folder) => {
    const name = draft.trim();
    setEditing(null);
    if (!name || name === f.name) return;
    if (serviceDesktop) {
      const revision = f.revision;
      if (revision) await change(() => mutate({ op: "folder_rename", folder_id: f.id, name, expected_revision: revision }));
    } else {
      await change(() => cmd.updateFolder(f.id, name, f.parent_id, f.default_template_id));
    }
  };

  const remove = async (f: Folder) => {
    if (!confirm(tr("folders.deleteConfirm", { name: f.name }))) return;
    const revision = f.revision;
    if (serviceDesktop && !revision) return;
    const deleted = await change(() => revision && serviceDesktop
      ? mutate({ op: "folder_delete", folder_id: f.id, expected_revision: revision })
      : cmd.deleteFolder(f.id));
    if (deleted && selection.kind === "folder" && selection.id === f.id) onSelect({ kind: "all" });
  };

  const row = (active: boolean, folderId?: string) =>
    cn(
      "focus-ring group flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-[13px] transition-colors",
      active ? "bg-canvas text-ink shadow-card" : "text-ink-2 hover:bg-canvas-3 hover:text-ink",
      folderId !== undefined && dragTarget === folderId && "bg-ember-soft ring-2 ring-ember",
    );

  const renderFolder = (f: Folder, depth: number) => {
    const active = selection.kind === "folder" && selection.id === f.id;
    return (
      <div key={f.id}>
        <div className={row(active, f.id)} style={{ paddingLeft: 8 + depth * 14 }} data-meeting-drop-target={f.id}>
          {editing === f.id ? (
            <form
              className="flex-1"
              onSubmit={(e) => {
                e.preventDefault();
                void rename(f);
              }}
            >
              <input aria-label={tr("folders.folderName")} autoFocus value={draft} onChange={(e) => setDraft(e.target.value)} onBlur={() => void rename(f)} className="w-full bg-transparent outline-none" />
            </form>
          ) : (
            <button className="flex min-w-0 flex-1 items-center gap-2" onClick={() => onSelect({ kind: "folder", id: f.id })}>
              <FolderOpen size={14} className="shrink-0 text-ink-3" />
              <span className="truncate">{f.name}</span>
              <span className="ml-auto text-[11px] text-ink-3">{f.meeting_count || ""}</span>
            </button>
          )}
          <span className="hidden shrink-0 items-center gap-0.5 group-hover:flex">
            <IconBtn title={tr("actions.rename")} onClick={() => { setEditing(f.id); setDraft(f.name); }}><Pencil size={11} /></IconBtn>
            <IconBtn title={tr("folders.newSubfolder")} onClick={() => void addFolder(f.id)}><FolderPlus size={11} /></IconBtn>
            <IconBtn title={tr("actions.delete")} onClick={() => void remove(f)}><Trash2 size={11} /></IconBtn>
          </span>
        </div>
        {children(f.id).map((c) => renderFolder(c, depth + 1))}
      </div>
    );
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-0.5">
        <button className={row(selection.kind === "all")} onClick={() => onSelect({ kind: "all" })}>
          <Layers size={14} className="text-ink-3" /> {tr("folders.allMeetings")}
        </button>
        <button className={row(selection.kind === "unfiled", "")} onClick={() => onSelect({ kind: "unfiled" })} data-meeting-drop-target="">
          <Inbox size={14} className="text-ink-3" /> {tr("folders.unfiled")}
        </button>
      </div>
      <div>
        <div className="mb-1 flex items-center justify-between px-2 text-[11px] font-semibold tracking-[0.12em] text-ink-3 uppercase">
          {tr("folders.title")}
          <button onClick={() => void addFolder(null)} className="text-ink-3 hover:text-ink" title={tr("folders.newFolder")}><FolderPlus size={13} /></button>
        </div>
        <div className="flex flex-col gap-0.5">
          {children(null).map((f) => renderFolder(f, 0))}
          {folders.length === 0 && <div className="px-2 text-[12px] text-ink-3">{tr("folders.empty")}</div>}
        </div>
      </div>
      {tags.length > 0 && (
        <div>
          <div className="mb-1 px-2 text-[11px] font-semibold tracking-[0.12em] text-ink-3 uppercase">{tr("folders.tags")}</div>
          <div className="flex flex-wrap gap-1 px-1">
            {tags.map((t) => {
              const active = selection.kind === "tag" && selection.id === t.id;
              return (
                <button key={t.id} onClick={() => onSelect(active ? { kind: "all" } : { kind: "tag", id: t.id })} className={cn("rounded-full px-2 py-0.5 text-[12px]", active ? "bg-ink text-canvas" : "bg-canvas-3 text-ink-2 hover:text-ink")}>
                  {t.name} <span className="opacity-60">{t.meeting_count}</span>
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

function IconBtn({ children, ...props }: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button className="grid h-5 w-5 place-items-center rounded text-ink-3 hover:bg-canvas-3 hover:text-ink" {...props}>
      {children}
    </button>
  );
}
