import { createSignal, createMemo, For, Show, type Component } from "solid-js";
import { store, type SessionModeString } from "../stores/dashboard";
import { api } from "../lib/api";

const MODE_LABELS: Record<SessionModeString, string> = {
  inactive: "Inactive",
  strict: "Strict",
  trust: "Trust",
  auto_accept: "Auto-Accept",
};

const MODE_CLASSES: Record<SessionModeString, string> = {
  inactive: "mode-inactive",
  strict: "mode-strict",
  trust: "mode-trust",
  auto_accept: "mode-auto",
};

const SessionPanel: Component = () => {
  const [selectedDevice, setSelectedDevice] = createSignal("");
  const [trustMins, setTrustMins] = createSignal(60);

  const activeDeviceId = () =>
    selectedDevice() || store.devices[0]?.deviceId || "";

  const sessions = createMemo(() => {
    const deviceSessions = store.sessionByDevice[activeDeviceId()];
    if (!deviceSessions) return [];
    return Object.values(deviceSessions);
  });

  const refreshSessions = async () => {
    try {
      await api.api.sessions.$get({
        query: { deviceId: activeDeviceId() },
      });
    } catch (e) {
      console.error("session query failed:", e);
    }
  };

  const setMode = async (callerUid: string, mode: SessionModeString) => {
    try {
      await api.api.session.mode.$post({
        json: {
          callerUid,
          mode,
          deviceId: activeDeviceId(),
          trustTimeoutMins: mode === "trust" ? trustMins() : undefined,
        },
      });
    } catch (e) {
      console.error("set session mode failed:", e);
    }
  };

  const trustRemaining = (expiresMs: number) => {
    if (!expiresMs) return "";
    const remaining = expiresMs - Date.now();
    if (remaining <= 0) return "expired";
    const m = Math.floor(remaining / 60000);
    const h = Math.floor(m / 60);
    return h > 0 ? `${h}h ${m % 60}m remaining` : `${m}m remaining`;
  };

  return (
    <div>
      <h2 class="panel-title">Sessions</h2>

      <Show
        when={store.devices.length > 0}
        fallback={
          <div class="empty-state">
            No devices connected. Connect a device to manage sessions.
          </div>
        }
      >
        <div class="flex gap-2 mb-2">
          <select
            value={selectedDevice()}
            onChange={(e) => setSelectedDevice(e.currentTarget.value)}
          >
            <For each={store.devices}>
              {(d) => <option value={d.deviceId}>{d.hostname}</option>}
            </For>
          </select>
          <button class="btn btn-sm" onClick={refreshSessions}>
            Refresh
          </button>
        </div>

        <Show
          when={sessions().length > 0}
          fallback={
            <div class="card">
              <div class="text-muted">
                No session data yet. Click Refresh to query.
              </div>
            </div>
          }
        >
          <For each={sessions()}>
            {(session) => (
              <div class="card">
                <div class="flex gap-2" style="align-items: center; margin-bottom: 12px">
                  <div class="card-title" style="margin-bottom: 0">
                    Caller: <span class="mono">{session.callerUid}</span>
                  </div>
                  <span class={`session-mode-badge ${MODE_CLASSES[session.mode]}`}>
                    {MODE_LABELS[session.mode]}
                  </span>
                </div>

                <Show when={session.mode === "trust" && session.trustExpiresMs > 0}>
                  <div class="text-sm text-muted mb-2">
                    {trustRemaining(session.trustExpiresMs)}
                    {" "}(timeout: {session.trustTimeoutMins}min)
                  </div>
                </Show>

                <div class="flex gap-2" style="flex-wrap: wrap">
                  <button
                    class={`btn btn-sm ${session.mode === "strict" ? "btn-active" : ""}`}
                    classList={{ "mode-strict-btn": session.mode !== "strict" }}
                    onClick={() => setMode(session.callerUid, "strict")}
                    disabled={session.mode === "strict"}
                  >
                    Strict
                  </button>
                  <div class="flex gap-2" style="align-items: center">
                    <button
                      class={`btn btn-sm ${session.mode === "trust" ? "btn-active" : ""}`}
                      classList={{ "mode-trust-btn": session.mode !== "trust" }}
                      onClick={() => setMode(session.callerUid, "trust")}
                      disabled={session.mode === "trust"}
                    >
                      Trust
                    </button>
                    <input
                      type="number"
                      min="1"
                      max="1440"
                      value={trustMins()}
                      onInput={(e) => setTrustMins(Number(e.currentTarget.value))}
                      style="width: 60px; padding: 4px 8px; font-size: 12px"
                    />
                    <span class="text-sm text-muted">min</span>
                  </div>
                  <button
                    class={`btn btn-sm ${session.mode === "auto_accept" ? "btn-active" : ""}`}
                    classList={{ "mode-auto-btn": session.mode !== "auto_accept" }}
                    onClick={() => setMode(session.callerUid, "auto_accept")}
                    disabled={session.mode === "auto_accept"}
                  >
                    Auto-Accept
                  </button>
                  <button
                    class="btn btn-sm"
                    onClick={() => setMode(session.callerUid, "inactive")}
                    disabled={session.mode === "inactive"}
                  >
                    Deactivate
                  </button>
                </div>
              </div>
            )}
          </For>
        </Show>
      </Show>
    </div>
  );
};

export default SessionPanel;
