"use client";

import { useEffect, useEffectEvent, useRef, useState } from "react";

export type DashboardEventPayload = {
  event: string;
  resource_type: string;
  resource_id: string;
  actor: string;
  detail: Record<string, unknown> | unknown[];
  timestamp: string;
};

type UseDashboardWsOptions = {
  fallbackIntervalMs?: number;
  reconnectDelayMs?: number;
  reconnectDelayMaxMs?: number;
  onEvent?: (event: DashboardEventPayload) => void;
  onFallback?: () => void;
};

type ConnectionState = "idle" | "connecting" | "open" | "closed" | "error";

export function useDashboardWs(options: UseDashboardWsOptions = {}) {
  const {
    fallbackIntervalMs = 20_000,
    reconnectDelayMs = 1_000,
    reconnectDelayMaxMs = 10_000,
    onEvent,
    onFallback,
  } = options;
  const [connectionState, setConnectionState] = useState<ConnectionState>(getInitialConnectionState);
  const [lastEvent, setLastEvent] = useState<DashboardEventPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const stateRef = useRef<ConnectionState>(getInitialConnectionState());
  const reconnectTimerRef = useRef<number | null>(null);
  const reconnectAttemptsRef = useRef(0);
  const sawOpenRef = useRef(false);
  const emitEvent = useEffectEvent((event: DashboardEventPayload) => {
    setLastEvent(event);
    setError(null);
    onEvent?.(event);
  });
  const emitFallback = useEffectEvent(() => {
    onFallback?.();
  });

  useEffect(() => {
    stateRef.current = connectionState;
  }, [connectionState]);

  useEffect(() => {
    let cancelled = false;
    let socket: WebSocket | null = null;
    const interval = window.setInterval(() => {
      if (stateRef.current !== "open") {
        emitFallback();
      }
    }, fallbackIntervalMs);

    const connect = () => {
      if (cancelled) {
        return;
      }

      stateRef.current = "connecting";
      setConnectionState("connecting");
      socket = new WebSocket(`${resolveWsBase()}/ws/dashboard`);

      socket.addEventListener("open", () => {
        reconnectAttemptsRef.current = 0;
        stateRef.current = "open";
        setConnectionState("open");
        setError(null);
        if (sawOpenRef.current) {
          emitFallback();
        }
        sawOpenRef.current = true;
      });
      socket.addEventListener("message", (message) => {
        try {
          const payload = JSON.parse(message.data) as DashboardEventPayload;
          if (payload.event === "system.resync") {
            emitFallback();
            return;
          }
          emitEvent(payload);
        } catch {
          setError("dashboard_event_parse_failed");
        }
      });
      socket.addEventListener("close", () => {
        stateRef.current = "closed";
        setConnectionState("closed");
        scheduleReconnect();
      });
      socket.addEventListener("error", () => {
        stateRef.current = "error";
        setConnectionState("error");
        setError("dashboard_ws_error");
        scheduleReconnect();
      });
    };

    const scheduleReconnect = () => {
      if (cancelled || reconnectTimerRef.current !== null) {
        return;
      }
      const delay = Math.min(
        reconnectDelayMs * 2 ** reconnectAttemptsRef.current,
        reconnectDelayMaxMs,
      );
      reconnectAttemptsRef.current += 1;
      reconnectTimerRef.current = window.setTimeout(() => {
        reconnectTimerRef.current = null;
        emitFallback();
        connect();
      }, delay);
    };

    connect();

    return () => {
      cancelled = true;
      window.clearInterval(interval);
      if (reconnectTimerRef.current !== null) {
        window.clearTimeout(reconnectTimerRef.current);
        reconnectTimerRef.current = null;
      }
      socket?.close();
    };
  }, [fallbackIntervalMs, reconnectDelayMaxMs, reconnectDelayMs]);

  return { connectionState, lastEvent, error };
}

function resolveWsBase() {
  return window.location.origin.replace(/^http/, "ws");
}

function getInitialConnectionState(): ConnectionState {
  if (typeof window === "undefined") {
    return "idle";
  }

  return "connecting";
}
