import { useCallback, useEffect, useRef, useState } from "react";
import { cmd, on, type RecordingBotConnectionStatus, type RecordingBotTarget } from "@/lib/ipc";

interface Connection {
  status: RecordingBotConnectionStatus;
  requested: boolean;
  disconnecting?: boolean;
  stop?: () => void;
}

export function useRecordingBotConnection() {
  const current = useRef<Connection | null>(null);
  const mounted = useRef(false);
  const [status, setStatus] = useState<RecordingBotConnectionStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [disconnecting, setDisconnecting] = useState(false);

  const release = useCallback((disconnect: boolean) => {
    const connection = current.current;
    current.current = null;
    connection?.stop?.();
    if (mounted.current) {
      setStatus(null);
      setError(null);
      setDisconnecting(false);
    }
    if (disconnect && connection?.requested) {
      void cmd.disconnectRecordingBot(connection.status.requestId).catch(() => {});
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      release(true);
    };
  }, [release]);

  async function connect(target: RecordingBotTarget) {
    if (current.current && !["failed", "disconnected"].includes(current.current.status.phase)) return;
    release(true);
    const requestId = crypto.randomUUID();
    const connection: Connection = { status: { requestId, revision: -1, phase: "connecting" }, requested: false };
    current.current = connection;
    setStatus(connection.status);

    function apply(update: RecordingBotConnectionStatus) {
      if (current.current !== connection || update.requestId !== requestId || update.revision <= connection.status.revision) return;
      connection.status = update;
      setStatus(update);
    }

    try {
      // Subscribe before sending the request. A fast admission event can precede the reply.
      const stop = await on("recording_bot_connection_status", apply);
      if (current.current !== connection) {
        stop();
        return;
      }
      connection.stop = stop;
      connection.requested = true;
      apply(await cmd.connectRecordingBot(requestId, target));
    } catch (detail) {
      if (current.current !== connection) return;
      release(true);
      setStatus({ ...connection.status, phase: "failed" });
      setError(detail);
    }
  }

  async function disconnect() {
    const connection = current.current;
    if (!connection || connection.disconnecting) return;
    if (!connection.requested) {
      release(false);
      return;
    }
    connection.disconnecting = true;
    setDisconnecting(true);
    setError(null);
    try {
      await cmd.disconnectRecordingBot(connection.status.requestId);
      if (current.current === connection) release(false);
    } catch (detail) {
      if (current.current === connection) {
        connection.disconnecting = false;
        setDisconnecting(false);
        setError(detail);
      }
    }
  }

  const reset = useCallback(() => release(true), [release]);

  return {
    status,
    error,
    disconnecting,
    connect,
    reset,
    disconnect,
    // Start reads the latest event even if a render has not reached the footer yet.
    connectedId: () => current.current?.status.phase === "connected" && !current.current.disconnecting
      ? current.current.status.requestId : undefined,
    adopted: () => release(false),
  };
}
