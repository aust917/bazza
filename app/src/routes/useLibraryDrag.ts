import { useCallback, useEffect, useRef, useState, type MouseEvent, type PointerEvent } from "react";

const dragThreshold = 6;

interface Gesture {
  pointerId: number;
  meetingId: string;
  folderId: string | null;
  x: number;
  y: number;
  capture?: { root: HTMLDivElement; x: number; y: number };
}

function beyondThreshold(gesture: Gesture, event: PointerEvent) {
  return Math.hypot(event.clientX - gesture.x, event.clientY - gesture.y) >= dragThreshold;
}

function dropTarget(root: HTMLDivElement, x: number, y: number, gesture: Gesture) {
  const target = root.ownerDocument.elementFromPoint(x, y)?.closest<HTMLElement>("[data-meeting-drop-target]");
  if (!target || !root.contains(target)) return;
  const id = target.dataset.meetingDropTarget;
  if ((id || null) !== gesture.folderId) return id;
}

/** Move within this window without relying on Windows' native drag session. */
export function useLibraryDrag(onMove: (meetingId: string, folderId: string | null) => void) {
  const gesture = useRef<Gesture | null>(null);
  const suppressMouseClick = useRef(false);
  const [preview, setPreview] = useState<{ meetingId: string; target?: string } | null>(null);

  const clear = useCallback(() => {
    const active = gesture.current;
    gesture.current = null;
    const root = active?.capture?.root;
    if (active && root?.hasPointerCapture?.(active.pointerId)) root.releasePointerCapture(active.pointerId);
    setPreview(null);
  }, []);

  const showTarget = useCallback(() => {
    const active = gesture.current;
    if (!active?.capture) return;
    const { root, x, y } = active.capture;
    const target = dropTarget(root, x, y, active);
    setPreview((previous) => previous?.meetingId === active.meetingId && previous.target === target
      ? previous : { meetingId: active.meetingId, target });
  }, []);

  useEffect(() => {
    const escape = (event: KeyboardEvent) => {
      if (event.key === "Escape" && gesture.current) {
        if (gesture.current.capture) event.preventDefault();
        clear();
      }
    };
    window.addEventListener("blur", clear);
    window.addEventListener("keydown", escape);
    window.addEventListener("scroll", showTarget, true);
    return () => {
      window.removeEventListener("blur", clear);
      window.removeEventListener("keydown", escape);
      window.removeEventListener("scroll", showTarget, true);
    };
  }, [clear, showTarget]);

  function begin(event: PointerEvent<HTMLAnchorElement>, meetingId: string, folderId: string | null) {
    if (event.pointerType !== "mouse" || !event.isPrimary || event.button !== 0
      || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
    gesture.current = { pointerId: event.pointerId, meetingId, folderId, x: event.clientX, y: event.clientY };
  }

  function onPointerMove(event: PointerEvent<HTMLDivElement>) {
    const active = gesture.current;
    if (!active || active.pointerId !== event.pointerId) return;
    if (!(event.buttons & 1)) {
      clear();
      return;
    }
    if (!active.capture) {
      if (!beyondThreshold(active, event)) return;
      active.capture = { root: event.currentTarget, x: event.clientX, y: event.clientY };
      active.capture.root.setPointerCapture?.(event.pointerId);
      suppressMouseClick.current = true;
    } else {
      active.capture.x = event.clientX;
      active.capture.y = event.clientY;
    }
    showTarget();
  }

  function onPointerUp(event: PointerEvent<HTMLDivElement>) {
    const active = gesture.current;
    if (!active || active.pointerId !== event.pointerId) return;
    const dragged = event.button === 0 && (active.capture || beyondThreshold(active, event));
    const target = dragged ? dropTarget(event.currentTarget, event.clientX, event.clientY, active) : undefined;
    if (dragged) suppressMouseClick.current = true;
    clear();
    if (target !== undefined) onMove(active.meetingId, target || null);
  }

  function cancel(event: PointerEvent<HTMLDivElement>) {
    if (gesture.current?.pointerId === event.pointerId) clear();
  }

  function onClickCapture(event: MouseEvent<HTMLDivElement>) {
    if (!suppressMouseClick.current || event.detail === 0) return;
    suppressMouseClick.current = false;
    event.preventDefault();
    event.stopPropagation();
  }

  return {
    begin,
    preview,
    handlers: {
      onPointerDownCapture(event: PointerEvent<HTMLDivElement>) {
        if (!event.isPrimary || event.button !== 0) return;
        if (gesture.current) clear();
        suppressMouseClick.current = false;
      },
      onPointerMove,
      onPointerUp,
      onPointerCancel: cancel,
      onLostPointerCapture: cancel,
      onPointerLeave(event: PointerEvent<HTMLDivElement>) { if (!gesture.current?.capture) cancel(event); },
      onClickCapture,
    },
  };
}
