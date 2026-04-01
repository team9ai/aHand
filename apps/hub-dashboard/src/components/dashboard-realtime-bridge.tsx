"use client";

import { startTransition, useRef } from "react";
import { useRouter } from "next/navigation";
import { useDashboardWs } from "@/hooks/use-dashboard-ws";

export function DashboardRealtimeBridge() {
  const router = useRouter();
  const lastRefreshAt = useRef(0);
  const refresh = () => {
    const now = Date.now();
    if (now - lastRefreshAt.current < 1_200) {
      return;
    }

    lastRefreshAt.current = now;
    startTransition(() => router.refresh());
  };

  useDashboardWs({
    onEvent: refresh,
    onFallback: refresh,
  });

  return null;
}
