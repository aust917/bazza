import { useTranslation } from "react-i18next";
import { useEffect, useId, useRef, useState, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes } from "react";
import { cn } from "@/lib/cn";
import { dbToLevel } from "@/lib/format";

import { Button } from "./Button";
export { Button };

export function Toggle({ checked, onChange, label, description, ariaLabel }: { checked: boolean; onChange: (v: boolean) => void; label: string; description?: string; ariaLabel?: string }) {
  return (
    <label className="flex cursor-pointer items-start justify-between gap-6 py-3">
      <span>
        <span className="block text-sm font-medium text-ink">{label}</span>
        {description && <span className="block text-[13px] text-ink-3">{description}</span>}
      </span>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={ariaLabel || label || undefined}
        onClick={() => onChange(!checked)}
        className={cn(
          "focus-ring relative mt-0.5 h-6 w-10 shrink-0 rounded-full transition-colors",
          checked ? "bg-ink" : "bg-line-2",
        )}
      >
        <span
          className={cn(
            "absolute top-0.5 left-0.5 h-5 w-5 rounded-full bg-canvas shadow-sm transition-transform",
            checked && "translate-x-4",
          )}
        />
      </button>
    </label>
  );
}

export function Select({ className, children, ...props }: SelectHTMLAttributes<HTMLSelectElement>) {
  return (
    <select
      className={cn(
        "focus-ring h-9 w-full appearance-none rounded-md border border-line-2 bg-canvas px-3 pr-8 text-sm text-ink",
        "bg-[url(\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 24 24' fill='none' stroke='%23888' stroke-width='2.5'%3E%3Cpath d='m6 9 6 6 6-6'/%3E%3C/svg%3E\")] bg-[length:12px] bg-[position:right_10px_center] bg-no-repeat",
        className,
      )}
      {...props}
    >
      {children}
    </select>
  );
}

export function Input({ className, ...props }: InputHTMLAttributes<HTMLInputElement>) {
  return (
    <input
      className={cn(
        "focus-ring h-9 w-full rounded-md border border-line-2 bg-canvas px-3 text-sm text-ink placeholder:text-ink-3",
        className,
      )}
      {...props}
    />
  );
}

export function Badge({ children, tone = "neutral", className }: { children: ReactNode; tone?: "neutral" | "ember" | "moss" | "amber"; className?: string }) {
  const tones = {
    neutral: "bg-canvas-3 text-ink-2",
    ember: "bg-ember-soft text-ember",
    moss: "bg-moss-soft text-moss",
    amber: "bg-amber-soft text-amber",
  };
  return (
    <span className={cn("inline-flex h-5 items-center rounded-full px-2 text-[11px] font-semibold tracking-wide uppercase", tones[tone], className)}>
      {children}
    </span>
  );
}

/** Horizontal level meter fed with dBFS. */
export function Meter({ db, label, className }: { db: number; label?: string; className?: string }) {
  const level = dbToLevel(db);
  const hot = db > -6;
  return (
    <div className={cn("flex items-center gap-2", className)}>
      {label && <span className="w-8 text-[11px] font-medium text-ink-3">{label}</span>}
      <div className="relative h-1.5 flex-1 overflow-hidden rounded-full bg-line">
        <div
          className={cn("absolute inset-y-0 left-0 rounded-full transition-[width] duration-100", hot ? "bg-ember" : "bg-moss")}
          style={{ width: `${Math.round(level * 100)}%` }}
        />
      </div>
    </div>
  );
}

export function Card({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cn("rounded-lg border border-line bg-canvas shadow-card", className)}>{children}</div>;
}

export function SectionTitle({ children, hint }: { children: ReactNode; hint?: ReactNode }) {
  return (
    <div className="mb-3 flex items-baseline justify-between">
      <h2 className="text-[11px] font-semibold tracking-[0.12em] text-ink-3 uppercase">{children}</h2>
      {hint && <span className="text-[12px] text-ink-3">{hint}</span>}
    </div>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return (
    <kbd className="rounded border border-line-2 bg-canvas-2 px-1.5 py-0.5 font-mono text-[11px] text-ink-2">{children}</kbd>
  );
}

export function Spinner({ className }: { className?: string }) {
  const { t: tr } = useTranslation("meeting");
  return (
    <span
      className={cn("inline-block h-4 w-4 animate-spin rounded-full border-2 border-line-2 border-t-ink", className)}
      aria-label={tr("common.loading")}
    />
  );
}

/** Action menu with keyboard navigation; closes on select, Escape, Tab or outside click. */
export function Menu({ label, items, className, disabled, inline = false }: {
  label: ReactNode;
  items: { label: ReactNode; onSelect: () => void; disabled?: boolean; danger?: boolean; checked?: boolean }[];
  className?: string;
  disabled?: boolean;
  inline?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const focusLast = useRef(false);
  const id = useId();
  const enabledItems = () => Array.from(menuRef.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? []);
  const focusEdge = (last: boolean) => {
    const enabled = enabledItems();
    (enabled[last ? enabled.length - 1 : 0] ?? menuRef.current)?.focus();
  };
  const close = (restoreFocus = false) => {
    setOpen(false);
    if (restoreFocus) ref.current?.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')?.focus();
  };
  useEffect(() => {
    if (!open) return;
    if (disabled) {
      setOpen(false);
      return;
    }
    focusEdge(focusLast.current);
    const onDoc = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close(true);
      }
    };
    // An inline list changes the layout when it closes. Let an outside click
    // reach its intended control before removing the list from the page.
    const outsideEvent = inline ? "click" : "mousedown";
    document.addEventListener(outsideEvent, onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener(outsideEvent, onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, disabled, inline]);
  return (
    <div ref={ref} className={cn("relative", className)}>
      <Button variant={inline ? "outline" : "ghost"} size={inline ? "lg" : "sm"}
        className={inline ? "w-full justify-between text-sm aria-expanded:border-ember/60" : undefined}
        id={`${id}-trigger`} aria-haspopup="menu" aria-expanded={open && !disabled} aria-controls={open && !disabled ? `${id}-menu` : undefined} disabled={disabled}
        onClick={() => { focusLast.current = false; setOpen((o) => !o); }}
        onKeyDown={(e) => {
          if (disabled || (e.key !== "ArrowDown" && e.key !== "ArrowUp")) return;
          e.preventDefault();
          focusLast.current = e.key === "ArrowUp";
          if (open) focusEdge(focusLast.current);
          else setOpen(true);
        }}>
        {label}
      </Button>
      {open && !disabled && (
        <div ref={menuRef} id={`${id}-menu`} role="menu" aria-labelledby={`${id}-trigger`} tabIndex={-1}
          onKeyDown={(e) => {
            if (e.key === "Tab") {
              // Return to the menu button before the browser advances focus normally.
              close(true);
              return;
            }
            const enabled = enabledItems();
            const current = enabled.indexOf(document.activeElement as HTMLButtonElement);
            let next: number;
            if (e.key === "ArrowDown") next = (current + 1) % enabled.length;
            else if (e.key === "ArrowUp") next = (current - 1 + enabled.length) % enabled.length;
            else if (e.key === "Home") next = 0;
            else if (e.key === "End") next = enabled.length - 1;
            else return;
            e.preventDefault();
            enabled[next]?.focus();
          }}
          className={cn("rounded-md border p-1", inline
            ? "mt-2 max-h-56 overflow-y-auto border-line-2 bg-canvas-2 shadow-card"
            : "absolute right-0 z-20 mt-1 w-max min-w-48 max-w-[min(26rem,calc(100vw-2rem))] border-line bg-canvas shadow-card")}>
          {items.map((it, i) => (
            <button
              key={i}
              type="button"
              role={it.checked === undefined ? "menuitem" : "menuitemradio"}
              aria-checked={it.checked}
              tabIndex={-1}
              disabled={it.disabled}
              onClick={() => {
                close(true);
                it.onSelect();
              }}
              className={cn(
                "focus-ring flex w-full items-center gap-2 rounded px-2.5 py-1.5 text-left text-[13px] wrap-anywhere disabled:opacity-40 [&_svg]:shrink-0",
                inline ? "min-h-9 hover:bg-canvas-3 aria-checked:bg-canvas-3" : "hover:bg-canvas-2",
                it.danger ? "text-ember" : "text-ink",
              )}
            >
              {it.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
