import { For, Show, type Component } from "solid-js";
import { store } from "../stores/dashboard";

const DevicesPanel: Component = () => {
  return (
    <div>
      <h2 class="panel-title">Devices</h2>
      <Show
        when={store.devices.length > 0}
        fallback={
          <div class="empty-state">
            No devices connected. Start a daemon to connect.
          </div>
        }
      >
        <For each={store.devices}>
          {(device) => (
            <div class="card">
              <div class="flex" style={{ "justify-content": "space-between", "align-items": "center" }}>
                <div>
                  <div class="card-title">{device.hostname}</div>
                  <div class="card-meta">
                    {device.os} &middot;{" "}
                    <span class="mono">{device.deviceId.slice(0, 12)}...</span>
                  </div>
                </div>
                <div class="status-label">
                  <span
                    class={`status-dot ${device.connected ? "connected" : "disconnected"}`}
                  />
                  {device.connected ? "connected" : "disconnected"}
                </div>
              </div>
              <Show when={device.capabilities.length > 0}>
                <div class="mt-2">
                  <For each={device.capabilities}>
                    {(cap) => <span class="tag">{cap}</span>}
                  </For>
                </div>
              </Show>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
};

export default DevicesPanel;
