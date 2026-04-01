"use client";

import { useEffect, useEffectEvent, useRef, useState } from "react";
import { readWsToken } from "@/lib/auth";

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
  onEvent?: (event: DashboardEventPayload) => void;
  onFallback?: () => void;
};

type ConnectionState = "idle" | "connecting" | "open" | "closed" | "error" | "unauthenticated";

export function useDashboardWs(options: UseDashboardWsOptions = {}) {
  const { fallbackIntervalMs = 20_000, onEvent, onFallback } = options;
  const [connectionState, setConnectionState] = useState<ConnectionState>(getInitialConnectionState);
  const [lastEvent, setLastEvent] = useState<DashboardEventPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const stateRef = useRef<ConnectionState>(getInitialConnectionState());
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
    const token = readWsToken();
    if (!token) {
      return;
    }

    const socket = new WebSocket(`${resolveWsBase()}/ws/dashboard?token=${encodeURIComponent(token)}`);

    socket.addEventListener("open", () => {
      stateRef.current = "open";
      setConnectionState("open");
      setError(null);
    });
    socket.addEventListener("message", (message) => {
      try {
        emitEvent(JSON.parse(message.data) as DashboardEventPayload);
      } catch {
        setError("dashboard_event_parse_failed");
      }
    });
    socket.addEventListener("close", () => {
      stateRef.current = "closed";
      setConnectionState("closed");
    });
    socket.addEventListener("error", () => {
      stateRef.current = "error";
      setConnectionState("error");
      setError("dashboard_ws_error");
    });

    const interval = window.setInterval(() => {
      if (stateRef.current !== "open") {
        emitFallback();
      }
    }, fallbackIntervalMs);

    return () => {
      window.clearInterval(interval);
      socket.close();
    };
  }, [fallbackIntervalMs]);

  return { connectionState, lastEvent, error };
}

function resolveWsBase() {
  if (process.env.NEXT_PUBLIC_AHAND_HUB_WS_BASE) {
    return process.env.NEXT_PUBLIC_AHAND_HUB_WS_BASE.replace(/\/$/, "");
  }

  return window.location.origin.replace(/^http/, "ws");
}

function getInitialConnectionState(): ConnectionState {
  if (typeof document === "undefined") {
    return "idle";
  }

  return readWsToken() ? "connecting" : "unauthenticated";
}
