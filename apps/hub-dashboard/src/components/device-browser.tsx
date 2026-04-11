"use client";

import { useCallback, useState } from "react";
import { buildProxyUrl } from "@/lib/hub-paths";

interface BrowserLogEntry {
  id: number;
  action: string;
  params?: Record<string, unknown>;
  success?: boolean;
  data?: unknown;
  error?: string;
  binaryData?: string;
  binaryMime?: string;
  loading: boolean;
  ts: number;
}

let nextEntryId = 0;

export function DeviceBrowser({ deviceId }: { deviceId: string }) {
  const [sessionId, setSessionId] = useState("test-session");
  const [url, setUrl] = useState("https://example.com");
  const [selector, setSelector] = useState("");
  const [fillValue, setFillValue] = useState("");
  const [customAction, setCustomAction] = useState("");
  const [customParams, setCustomParams] = useState("{}");
  const [log, setLog] = useState<BrowserLogEntry[]>([]);
  const [showCustom, setShowCustom] = useState(false);

  const addEntry = useCallback(
    (action: string, params?: Record<string, unknown>): number => {
      const id = nextEntryId++;
      setLog((prev) => [
        { id, action, params, loading: true, ts: Date.now() },
        ...prev,
      ]);
      return id;
    },
    [],
  );

  const updateEntry = useCallback(
    (
      id: number,
      result: {
        success?: boolean;
        data?: unknown;
        error?: string;
        binary_data?: string;
        binary_mime?: string;
      },
    ) => {
      setLog((prev) =>
        prev.map((e) =>
          e.id === id
            ? {
                ...e,
                success: result.success,
                data: result.data,
                error: result.error,
                binaryData: result.binary_data,
                binaryMime: result.binary_mime,
                loading: false,
              }
            : e,
        ),
      );
    },
    [],
  );

  const send = useCallback(
    async (action: string, params?: Record<string, unknown>) => {
      const entryId = addEntry(action, params);
      try {
        const res = await fetch(buildProxyUrl("/api/browser"), {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({
            device_id: deviceId,
            session_id: sessionId,
            action,
            params,
          }),
        });
        const data = await res.json();
        updateEntry(entryId, data);
      } catch (e) {
        updateEntry(entryId, {
          success: false,
          error: e instanceof Error ? e.message : String(e),
        });
      }
    },
    [deviceId, sessionId, addEntry, updateEntry],
  );

  const handleOpen = () => {
    if (!url.trim()) return;
    send("open", { url: url.trim() });
  };
  const handleSnapshot = () => send("snapshot");
  const handleClick = () => {
    if (!selector.trim()) return;
    send("click", { selector: selector.trim() });
  };
  const handleFill = () => {
    if (!selector.trim()) return;
    send("fill", { selector: selector.trim(), value: fillValue });
  };
  const handleScreenshot = () => send("screenshot");
  const handleDownload = () => {
    if (!selector.trim()) return;
    send("download", { selector: selector.trim() });
  };
  const handlePdf = () => send("pdf");
  const handleClose = () => send("close");

  const handleCustom = () => {
    if (!customAction.trim()) return;
    try {
      const params = JSON.parse(customParams);
      send(customAction.trim(), params);
    } catch {
      const entryId = addEntry(customAction.trim());
      updateEntry(entryId, { success: false, error: "Invalid JSON in params" });
    }
  };

  const makeBlobUrl = (base64: string, mime: string): string => {
    const raw = atob(base64);
    const bytes = new Uint8Array(raw.length);
    for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
    return URL.createObjectURL(new Blob([bytes], { type: mime }));
  };

  const extractFilename = (entry: BrowserLogEntry): string => {
    const path = (entry.data as Record<string, unknown>)?.path;
    if (typeof path === "string") {
      const parts = path.split("/");
      return parts[parts.length - 1] || `${entry.action}-result`;
    }
    return `${entry.action}-result`;
  };

  return (
    <div className="browser-panel">
      <div className="browser-section">
        <div className="browser-section-title">Session</div>
        <div className="browser-form-row">
          <label className="browser-label">Session ID</label>
          <input
            className="browser-input"
            value={sessionId}
            onChange={(e) => setSessionId(e.target.value)}
            placeholder="browser session identifier"
          />
        </div>
      </div>

      <div className="browser-section">
        <div className="browser-section-title">Actions</div>
        <div className="browser-form-row">
          <label className="browser-label">URL</label>
          <input
            className="browser-input"
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            placeholder="https://example.com"
            onKeyDown={(e) => e.key === "Enter" && handleOpen()}
          />
          <button className="browser-btn browser-btn-primary" onClick={handleOpen}>
            Open
          </button>
        </div>
        <div className="browser-form-row">
          <label className="browser-label">Selector</label>
          <input
            className="browser-input"
            value={selector}
            onChange={(e) => setSelector(e.target.value)}
            placeholder="@e2 or CSS selector"
          />
        </div>
        <div className="browser-form-row">
          <label className="browser-label">Value</label>
          <input
            className="browser-input"
            value={fillValue}
            onChange={(e) => setFillValue(e.target.value)}
            placeholder="text for fill action"
            onKeyDown={(e) => e.key === "Enter" && handleFill()}
          />
        </div>
        <div className="browser-actions-row">
          <button className="browser-btn" onClick={handleSnapshot}>Snapshot</button>
          <button className="browser-btn" onClick={handleClick} disabled={!selector.trim()}>Click</button>
          <button className="browser-btn" onClick={handleFill} disabled={!selector.trim()}>Fill</button>
          <button className="browser-btn" onClick={handleScreenshot}>Screenshot</button>
          <button className="browser-btn" onClick={handleDownload} disabled={!selector.trim()}>Download</button>
          <button className="browser-btn" onClick={handlePdf}>PDF</button>
          <button className="browser-btn browser-btn-danger" onClick={handleClose}>Close</button>
        </div>
      </div>

      <div className="browser-section">
        <div className="browser-section-header">
          <span className="browser-section-title">Custom Command</span>
          <button className="browser-btn browser-btn-sm" onClick={() => setShowCustom(!showCustom)}>
            {showCustom ? "Hide" : "Show"}
          </button>
        </div>
        {showCustom && (
          <>
            <div className="browser-form-row">
              <label className="browser-label">Action</label>
              <input
                className="browser-input"
                value={customAction}
                onChange={(e) => setCustomAction(e.target.value)}
                placeholder="e.g. hover, select, drag"
              />
            </div>
            <div className="browser-form-row">
              <label className="browser-label">Params</label>
              <input
                className="browser-input"
                value={customParams}
                onChange={(e) => setCustomParams(e.target.value)}
                placeholder='{"key": "value"}'
                onKeyDown={(e) => e.key === "Enter" && handleCustom()}
              />
            </div>
            <button className="browser-btn browser-btn-primary" onClick={handleCustom}>
              Send
            </button>
          </>
        )}
      </div>

      <div className="browser-section">
        <div className="browser-section-header">
          <span className="browser-section-title">
            Response Log ({log.length})
          </span>
          {log.length > 0 && (
            <button className="browser-btn browser-btn-sm" onClick={() => setLog([])}>
              Clear
            </button>
          )}
        </div>
        {log.length === 0 ? (
          <p className="empty-state">No browser commands sent yet.</p>
        ) : (
          <div className="browser-log">
            {log.map((entry) => (
              <div className="browser-log-entry" key={entry.id}>
                <div className="browser-log-header">
                  <span className="browser-log-action">{entry.action}</span>
                  {entry.params && (
                    <span className="browser-log-params">
                      {JSON.stringify(entry.params)}
                    </span>
                  )}
                  <span className="browser-log-time">
                    {new Date(entry.ts).toLocaleTimeString()}
                  </span>
                </div>
                <div className="browser-log-body">
                  {entry.loading ? (
                    <span className="browser-log-loading">Loading...</span>
                  ) : (
                    <>
                      {entry.success !== undefined && (
                        <span className={entry.success ? "browser-log-success" : "browser-log-fail"}>
                          {entry.success ? "SUCCESS" : "FAILED"}
                        </span>
                      )}
                      {entry.error && (
                        <span className="browser-log-fail"> {entry.error}</span>
                      )}
                      {entry.binaryData && entry.binaryMime && (
                        <div className="browser-log-binary">
                          {entry.binaryMime.startsWith("image/") ? (
                            <img
                              className="browser-preview-img"
                              src={`data:${entry.binaryMime};base64,${entry.binaryData}`}
                              alt={extractFilename(entry)}
                            />
                          ) : (
                            <a
                              className="browser-download-link"
                              href={makeBlobUrl(entry.binaryData, entry.binaryMime)}
                              download={extractFilename(entry)}
                            >
                              Download {extractFilename(entry)} ({entry.binaryMime})
                            </a>
                          )}
                        </div>
                      )}
                      {entry.data !== undefined && entry.data !== null && (
                        <pre className="browser-log-data">
                          {typeof entry.data === "string"
                            ? entry.data
                            : JSON.stringify(entry.data, null, 2)}
                        </pre>
                      )}
                    </>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
