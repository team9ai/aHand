import { createResource, createSignal, Show, onMount } from "solid-js";
import { api, StatusResponse } from "../lib/api";

const GITHUB_REPO = "team9ai/aHand";

export default function StatusPanel() {
  const [status] = createResource<StatusResponse>(api.getStatus);
  const [latestVersion, setLatestVersion] = createSignal<string | null>(null);

  onMount(async () => {
    try {
      const resp = await fetch(`https://api.github.com/repos/${GITHUB_REPO}/releases/latest`);
      if (resp.ok) {
        const data = await resp.json();
        setLatestVersion(data.tag_name?.replace(/^v/, "") || null);
      }
    } catch {}
  });

  function formatBytes(bytes: number): string {
    if (bytes === 0) return "0 B";
    const k = 1024;
    const sizes = ["B", "KB", "MB", "GB"];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return Math.round(bytes / Math.pow(k, i) * 100) / 100 + " " + sizes[i];
  }

  function hasUpdate(): boolean {
    const current = status()?.version;
    const latest = latestVersion();
    return !!current && !!latest && current !== latest;
  }

  return (
    <div class="panel">
      <h2>Daemon Status</h2>
      <Show when={status.loading}>
        <p>Loading...</p>
      </Show>
      <Show when={status.error}>
        <p class="error">Error: {status.error.message}</p>
      </Show>
      <Show when={status()}>
        {(data) => (
          <div class="status-grid">
            <div class="status-item">
              <span class="label">Version</span>
              <span class="value">
                v{data().version}
                <Show when={hasUpdate()}>
                  {" "}
                  <span class="update-badge">Update available: v{latestVersion()}</span>
                </Show>
              </span>
            </div>
            <div class="status-item">
              <span class="label">Daemon</span>
              <span class={data().daemon_running ? "value success" : "value error"}>
                {data().daemon_running ? "Running" : "Stopped"}
                {data().daemon_pid && ` (PID: ${data().daemon_pid})`}
              </span>
            </div>
            <div class="status-item">
              <span class="label">Config Path</span>
              <span class="value">{data().config_path}</span>
            </div>
            <div class="status-item">
              <span class="label">Data Directory</span>
              <span class="value">{data().data_dir}</span>
            </div>
            <div class="status-item">
              <span class="label">Data Directory Size</span>
              <span class="value">{formatBytes(data().data_dir_size)}</span>
            </div>
          </div>
        )}
      </Show>
    </div>
  );
}
