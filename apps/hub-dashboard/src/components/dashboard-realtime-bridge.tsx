"use client";

import { startTransition, useEffect, useRef } from "react";
import { useRouter } from "next/navigation";
import { useDashboardWs } from "@/hooks/use-dashboard-ws";

export function DashboardRealtimeBridge() {
  const router = useRouter();
  const lastRefreshAt = useRef(0);
  const trailingRefreshRef = useRef<number | null>(null);
  const refresh = () => {
    const now = Date.now();
    const remaining = 1_200 - (now - lastRefreshAt.current);
    if (remaining > 0) {
      if (trailingRefreshRef.current === null) {
        trailingRefreshRef.current = window.setTimeout(() => {
          trailingRefreshRef.current = null;
          lastRefreshAt.current = Date.now();
          startTransition(() => router.refresh());
        }, remaining);
      }
      return;
    }

    lastRefreshAt.current = now;
    startTransition(() => router.refresh());
  };

  useEffect(() => () => {
    if (trailingRefreshRef.current !== null) {
      window.clearTimeout(trailingRefreshRef.current);
    }
  }, []);

  useDashboardWs({
    onEvent: refresh,
    onFallback: refresh,
  });

  return null;
}
