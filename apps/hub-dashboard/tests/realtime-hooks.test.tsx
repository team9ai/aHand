import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DashboardRealtimeBridge } from "@/components/dashboard-realtime-bridge";
import { useDashboardWs } from "@/hooks/use-dashboard-ws";
import { useJobOutput } from "@/hooks/use-job-output";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ refresh: refreshMock }),
}));

const refreshMock = vi.fn();

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];

  url: string;
  listeners = new Map<string, Set<(event?: unknown) => void>>();
  close = vi.fn(() => {
    this.emit("close");
  });

  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type: string, listener: (event?: unknown) => void) {
    const set = this.listeners.get(type) ?? new Set();
    set.add(listener);
    this.listeners.set(type, set);
  }

  emit(type: string, event?: unknown) {
    for (const listener of this.listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

class FakeEventSource {
  static instances: FakeEventSource[] = [];

  url: string;
  listeners = new Map<string, Set<(event?: unknown) => void>>();
  close = vi.fn();
  onerror: (() => void) | null = null;

  constructor(url: string) {
    this.url = url;
    FakeEventSource.instances.push(this);
  }

  addEventListener(type: string, listener: (event?: unknown) => void) {
    const set = this.listeners.get(type) ?? new Set();
    set.add(listener);
    this.listeners.set(type, set);
  }

  emit(type: string, data: string) {
    for (const listener of this.listeners.get(type) ?? []) {
      listener({ data });
    }
  }
}

function DashboardHookHarness({
  fallbackIntervalMs = 20_000,
  reconnectDelayMs = 250,
}: {
  fallbackIntervalMs?: number;
  reconnectDelayMs?: number;
}) {
  const state = useDashboardWs({ fallbackIntervalMs, reconnectDelayMs });
  return <div>{state.connectionState}</div>;
}

function JobOutputHarness({ jobId }: { jobId: string }) {
  const { entries, status, error } = useJobOutput(jobId);
  return (
    <div>
      <div data-testid="status">{status}</div>
      <div data-testid="error">{error ?? ""}</div>
      <div data-testid="entries">{entries.map((entry) => entry.text).join("|")}</div>
    </div>
  );
}

describe("realtime hooks", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.stubGlobal("WebSocket", FakeWebSocket as unknown as typeof WebSocket);
    vi.stubGlobal("EventSource", FakeEventSource as unknown as typeof EventSource);
    FakeWebSocket.instances = [];
    FakeEventSource.instances = [];
    refreshMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
    vi.clearAllMocks();
  });

  it("uses same-origin dashboard websocket auth and reconnects after disconnect", async () => {
    render(<DashboardHookHarness reconnectDelayMs={200} />);

    expect(FakeWebSocket.instances[0]?.url).toBe("ws://localhost:3000/ws/dashboard");

    act(() => {
      FakeWebSocket.instances[0]?.emit("open");
      FakeWebSocket.instances[0]?.emit("close");
      vi.advanceTimersByTime(210);
    });

    expect(FakeWebSocket.instances).toHaveLength(2);
  });

  it("schedules a trailing refresh for burst dashboard events", async () => {
    render(<DashboardRealtimeBridge />);

    act(() => {
      FakeWebSocket.instances[0]?.emit("open");
      FakeWebSocket.instances[0]?.emit("message", {
        data: JSON.stringify({
          event: "job.running",
          resource_type: "job",
          resource_id: "job-1",
          actor: "device:device-1",
          detail: {},
          timestamp: "2026-04-02T09:00:00Z",
        }),
      });
      FakeWebSocket.instances[0]?.emit("message", {
        data: JSON.stringify({
          event: "job.finished",
          resource_type: "job",
          resource_id: "job-1",
          actor: "device:device-1",
          detail: {},
          timestamp: "2026-04-02T09:00:01Z",
        }),
      });
    });

    expect(refreshMock).toHaveBeenCalledTimes(1);

    act(() => {
      vi.advanceTimersByTime(1_250);
    });

    expect(refreshMock).toHaveBeenCalledTimes(2);
  });

  it("refreshes immediately when the dashboard stream requests a resync", async () => {
    render(<DashboardRealtimeBridge />);

    act(() => {
      FakeWebSocket.instances[0]?.emit("open");
      FakeWebSocket.instances[0]?.emit("message", {
        data: JSON.stringify({
          event: "system.resync",
          resource_type: "system",
          resource_id: "dashboard",
          actor: "hub",
          detail: { reason: "lagged" },
          timestamp: "2026-04-02T09:00:02Z",
        }),
      });
    });

    expect(refreshMock).toHaveBeenCalledTimes(1);
  });

  it("keeps job output streaming alive across transient event source errors", async () => {
    render(<JobOutputHarness jobId="job-1" />);

    expect(FakeEventSource.instances[0]?.url).toBe("/api/proxy/api/jobs/job-1/output");

    act(() => {
      FakeEventSource.instances[0]?.emit("stdout", "hello");
      FakeEventSource.instances[0]?.onerror?.();
    });

    expect(FakeEventSource.instances[0]?.close).not.toHaveBeenCalled();
    expect(screen.getByTestId("error")).toHaveTextContent(/live output connection lost/i);

    act(() => {
      FakeEventSource.instances[0]?.emit("stdout", "again");
    });

    expect(screen.getByTestId("entries")).toHaveTextContent("hello|again");
  });

  it("reports an error state when the websocket emits an error event", async () => {
    function ErrorHarness() {
      const { connectionState, error } = useDashboardWs({ reconnectDelayMs: 200 });
      return (
        <div>
          <div data-testid="state">{connectionState}</div>
          <div data-testid="error">{error ?? ""}</div>
        </div>
      );
    }

    render(<ErrorHarness />);

    act(() => {
      FakeWebSocket.instances[0]?.emit("error");
    });

    expect(screen.getByTestId("state")).toHaveTextContent("error");
    expect(screen.getByTestId("error")).toHaveTextContent("dashboard_ws_error");
  });

  it("reports a parse error when a websocket message contains invalid JSON", async () => {
    function ParseErrorHarness() {
      const { error } = useDashboardWs({ reconnectDelayMs: 200 });
      return <div data-testid="error">{error ?? ""}</div>;
    }

    render(<ParseErrorHarness />);

    act(() => {
      FakeWebSocket.instances[0]?.emit("open");
      FakeWebSocket.instances[0]?.emit("message", { data: "not-json{{{" });
    });

    expect(screen.getByTestId("error")).toHaveTextContent("dashboard_event_parse_failed");
  });

  it("falls back to generic finished message when finished event has invalid JSON", async () => {
    render(<JobOutputHarness jobId="job-1" />);

    act(() => {
      FakeEventSource.instances[0]?.emit("finished", "not-valid-json");
    });

    expect(screen.getByTestId("entries")).toHaveTextContent("Command finished");
    expect(screen.getByTestId("status")).toHaveTextContent("complete");
  });

  it("does not overwrite complete status when event source errors after finishing", async () => {
    render(<JobOutputHarness jobId="job-1" />);

    act(() => {
      FakeEventSource.instances[0]?.emit("finished", JSON.stringify({ exit_code: 0, error: "" }));
    });

    expect(screen.getByTestId("status")).toHaveTextContent("complete");

    act(() => {
      FakeEventSource.instances[0]?.onerror?.();
    });

    expect(screen.getByTestId("status")).toHaveTextContent("complete");
  });
});
