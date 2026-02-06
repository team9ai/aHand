import { createSignal, For, Show, type Component } from "solid-js";
import { store } from "../stores/dashboard";
import { api } from "../lib/api";

interface BrowserLogEntry {
  id: number;
  action: string;
  params?: Record<string, unknown>;
  success?: boolean;
  data?: unknown;
  error?: string;
  binaryData?: string; // base64
  binaryMime?: string;
  loading: boolean;
  ts: number;
}

let nextId = 0;

const BrowserPanel: Component = () => {
  const [sessionId, setSessionId] = createSignal("test-session");
  const [deviceId, setDeviceId] = createSignal("");
  const [url, setUrl] = createSignal("https://example.com");
  const [selector, setSelector] = createSignal("");
  const [fillValue, setFillValue] = createSignal("");
  const [customAction, setCustomAction] = createSignal("");
  const [customParams, setCustomParams] = createSignal("{}");
  const [log, setLog] = createSignal<BrowserLogEntry[]>([]);
  const [showCustom, setShowCustom] = createSignal(false);

  const addEntry = (action: string, params?: Record<string, unknown>): number => {
    const id = nextId++;
    setLog((prev) => [
      { id, action, params, loading: true, ts: Date.now() },
      ...prev,
    ]);
    return id;
  };

  const updateEntry = (id: number, result: { success?: boolean; data?: unknown; error?: string; binaryData?: string; binaryMime?: string }) => {
    setLog((prev) =>
      prev.map((e) => (e.id === id ? { ...e, ...result, loading: false } : e)),
    );
  };

  const send = async (action: string, params?: Record<string, unknown>) => {
    const entryId = addEntry(action, params);
    try {
      const res = await api.api.browser.$post({
        json: {
          sessionId: sessionId(),
          action,
          params,
          deviceId: deviceId() || undefined,
        },
      });
      const data = await res.json() as { success?: boolean; data?: unknown; error?: string; binaryData?: string; binaryMime?: string };
      updateEntry(entryId, data);
    } catch (e) {
      updateEntry(entryId, { success: false, error: e instanceof Error ? e.message : String(e) });
    }
  };

  const handleOpen = () => {
    if (!url().trim()) return;
    send("open", { url: url().trim() });
  };

  const handleSnapshot = () => send("snapshot");

  const handleClick = () => {
    if (!selector().trim()) return;
    send("click", { selector: selector().trim() });
  };

  const handleFill = () => {
    if (!selector().trim()) return;
    send("fill", { selector: selector().trim(), value: fillValue() });
  };

  const handleScreenshot = () => send("screenshot");

  const handleDownload = () => {
    if (!selector().trim()) return;
    send("download", { selector: selector().trim() });
  };

  const handlePdf = () => send("pdf");

  const handleClose = () => send("close");

  const handleCustom = () => {
    if (!customAction().trim()) return;
    try {
      const params = JSON.parse(customParams());
      send(customAction().trim(), params);
    } catch {
      const entryId = addEntry(customAction().trim());
      updateEntry(entryId, { success: false, error: "Invalid JSON in params" });
    }
  };

  /** Create a blob download URL from base64 + mime. */
  const makeBlobUrl = (base64: string, mime: string): string => {
    const raw = atob(base64);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return URL.createObjectURL(new Blob([bytes], { type: mime }));
  };

  /** Extract a filename from the data.path field, or generate a fallback. */
  const extractFilename = (entry: BrowserLogEntry): string => {
    const path = (entry.data as Record<string, unknown>)?.path;
    if (typeof path === "string") {
      const parts = path.split("/");
      return parts[parts.length - 1] || `${entry.action}-result`;
    }
    return `${entry.action}-result`;
  };

  const clearLog = () => setLog([]);

  return (
    <div>
      <h2 class="panel-title">Browser</h2>

      {/* Session config */}
      <div class="card">
        <div class="card-title">Session</div>
        <Show when={store.devices.length > 1}>
          <div class="form-row">
            <span class="form-label">Device</span>
            <select
              value={deviceId()}
              onChange={(e) => setDeviceId(e.currentTarget.value)}
            >
              <option value="">Auto (first device)</option>
              <For each={store.devices}>
                {(d) => <option value={d.deviceId}>{d.hostname}</option>}
              </For>
            </select>
          </div>
        </Show>
        <div class="form-row">
          <span class="form-label">Session ID</span>
          <input
            type="text"
            value={sessionId()}
            onInput={(e) => setSessionId(e.currentTarget.value)}
            placeholder="browser session identifier"
          />
        </div>
      </div>

      {/* Quick actions */}
      <div class="card">
        <div class="card-title">Actions</div>

        {/* Open URL */}
        <div class="form-row">
          <span class="form-label">URL</span>
          <input
            type="text"
            value={url()}
            onInput={(e) => setUrl(e.currentTarget.value)}
            placeholder="https://example.com"
            onKeyDown={(e) => e.key === "Enter" && handleOpen()}
          />
          <button class="btn btn-primary btn-sm" onClick={handleOpen}>
            Open
          </button>
        </div>

        {/* Selector + actions */}
        <div class="form-row">
          <span class="form-label">Selector</span>
          <input
            type="text"
            value={selector()}
            onInput={(e) => setSelector(e.currentTarget.value)}
            placeholder="@e2 or CSS selector"
          />
        </div>

        <div class="form-row">
          <span class="form-label">Value</span>
          <input
            type="text"
            value={fillValue()}
            onInput={(e) => setFillValue(e.currentTarget.value)}
            placeholder="text for fill action"
            onKeyDown={(e) => e.key === "Enter" && handleFill()}
          />
        </div>

        <div class="browser-actions">
          <button class="btn btn-sm" onClick={handleSnapshot}>Snapshot</button>
          <button class="btn btn-sm" onClick={handleClick} disabled={!selector().trim()}>Click</button>
          <button class="btn btn-sm" onClick={handleFill} disabled={!selector().trim()}>Fill</button>
          <button class="btn btn-sm" onClick={handleScreenshot}>Screenshot</button>
          <button class="btn btn-sm" onClick={handleDownload} disabled={!selector().trim()}>Download</button>
          <button class="btn btn-sm" onClick={handlePdf}>PDF</button>
          <button class="btn btn-sm btn-danger" onClick={handleClose}>Close</button>
        </div>
      </div>

      {/* Custom command */}
      <div class="card">
        <div class="flex" style="justify-content: space-between; align-items: center; margin-bottom: 8px">
          <div class="card-title" style="margin-bottom: 0">Custom Command</div>
          <button
            class="btn btn-sm"
            onClick={() => setShowCustom(!showCustom())}
          >
            {showCustom() ? "Hide" : "Show"}
          </button>
        </div>
        <Show when={showCustom()}>
          <div class="form-row">
            <span class="form-label">Action</span>
            <input
              type="text"
              value={customAction()}
              onInput={(e) => setCustomAction(e.currentTarget.value)}
              placeholder="e.g. hover, select, drag"
            />
          </div>
          <div class="form-row">
            <span class="form-label">Params</span>
            <input
              type="text"
              value={customParams()}
              onInput={(e) => setCustomParams(e.currentTarget.value)}
              placeholder='{"key": "value"}'
              onKeyDown={(e) => e.key === "Enter" && handleCustom()}
            />
          </div>
          <button class="btn btn-primary btn-sm" onClick={handleCustom}>
            Send
          </button>
        </Show>
      </div>

      {/* Response log */}
      <div class="flex" style="justify-content: space-between; align-items: center; margin-bottom: 8px">
        <h3 style="font-size: 14px; font-weight: 600">
          Response Log ({log().length})
        </h3>
        <Show when={log().length > 0}>
          <button class="btn btn-sm" onClick={clearLog}>Clear</button>
        </Show>
      </div>

      <Show
        when={log().length > 0}
        fallback={<div class="empty-state">No browser commands sent yet.</div>}
      >
        <For each={log()}>
          {(entry) => (
            <div class="browser-log-entry">
              <div class="browser-log-header">
                <span class="browser-log-action">{entry.action}</span>
                <Show when={entry.params}>
                  <span class="browser-log-params mono">
                    {JSON.stringify(entry.params)}
                  </span>
                </Show>
                <span class="text-muted text-sm" style="margin-left: auto">
                  {new Date(entry.ts).toLocaleTimeString()}
                </span>
              </div>
              <Show when={entry.loading}>
                <div class="browser-log-body text-muted">Loading...</div>
              </Show>
              <Show when={!entry.loading}>
                <div class="browser-log-body">
                  <Show when={entry.success !== undefined}>
                    <span class={entry.success ? "text-success" : "text-danger"}>
                      {entry.success ? "SUCCESS" : "FAILED"}
                    </span>
                  </Show>
                  <Show when={entry.error}>
                    <span class="text-danger"> {entry.error}</span>
                  </Show>
                  {/* Binary data preview */}
                  <Show when={entry.binaryData && entry.binaryMime}>
                    <div class="mt-2">
                      <Show when={entry.binaryMime?.startsWith("image/")}>
                        <img
                          class="browser-preview-img"
                          src={`data:${entry.binaryMime};base64,${entry.binaryData}`}
                          alt={extractFilename(entry)}
                        />
                      </Show>
                      <Show when={!entry.binaryMime?.startsWith("image/")}>
                        <a
                          class="browser-download-link"
                          href={makeBlobUrl(entry.binaryData!, entry.binaryMime!)}
                          download={extractFilename(entry)}
                        >
                          Download {extractFilename(entry)} ({entry.binaryMime})
                        </a>
                      </Show>
                    </div>
                  </Show>
                  <Show when={entry.data !== undefined && entry.data !== null}>
                    <pre class="browser-log-data">
                      {typeof entry.data === "string"
                        ? entry.data
                        : JSON.stringify(entry.data, null, 2)}
                    </pre>
                  </Show>
                </div>
              </Show>
            </div>
          )}
        </For>
      </Show>
    </div>
  );
};

export default BrowserPanel;
