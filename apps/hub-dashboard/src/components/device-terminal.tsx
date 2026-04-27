"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { buildProxyUrl } from "@/lib/hub-paths";

/* ------------------------------------------------------------------ */
/*  Shared types                                                      */
/* ------------------------------------------------------------------ */

type TerminalEntry =
  | { kind: "command"; tool: string; text: string }
  | { kind: "stdout"; text: string }
  | { kind: "stderr"; text: string }
  | { kind: "exit"; code: number; error?: string }
  | { kind: "error"; text: string };

/* ------------------------------------------------------------------ */
/*  Main export – switches between pipe and interactive                */
/* ------------------------------------------------------------------ */

export function DeviceTerminal({ deviceId }: { deviceId: string }) {
  const [interactive, setInteractive] = useState(false);

  if (interactive) {
    return (
      <InteractiveTerminal
        deviceId={deviceId}
        onSwitchMode={() => setInteractive(false)}
      />
    );
  }

  return (
    <PipeTerminal
      deviceId={deviceId}
      onSwitchMode={() => setInteractive(true)}
    />
  );
}

/* ------------------------------------------------------------------ */
/*  PipeTerminal – existing pipe-mode terminal                         */
/* ------------------------------------------------------------------ */

function PipeTerminal({
  deviceId,
  onSwitchMode,
}: {
  deviceId: string;
  onSwitchMode: () => void;
}) {
  const [entries, setEntries] = useState<TerminalEntry[]>([]);
  const [input, setInput] = useState("");
  const [tool, setTool] = useState("bash");
  const [running, setRunning] = useState(false);
  const outputRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  const scrollToBottom = useCallback(() => {
    if (outputRef.current) {
      outputRef.current.scrollTop = outputRef.current.scrollHeight;
    }
  }, []);

  useEffect(scrollToBottom, [entries, scrollToBottom]);

  const push = useCallback((entry: TerminalEntry) => {
    setEntries((prev) => [...prev, entry]);
  }, []);

  const execCommand = useCallback(async () => {
    const cmd = input.trim();
    if (!cmd || running) return;

    setInput("");
    setRunning(true);
    push({ kind: "command", tool, text: cmd });

    const argFlag = tool === "node" ? "-e" : "-c";

    let jobId: string;
    try {
      const res = await fetch(buildProxyUrl("/api/jobs"), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          device_id: deviceId,
          tool,
          args: [argFlag, cmd],
          timeout_ms: 60000,
        }),
      });
      if (!res.ok) {
        const body = await res.text();
        push({ kind: "error", text: `Job creation failed (${res.status}): ${body}` });
        setRunning(false);
        return;
      }
      const data = (await res.json()) as { job_id: string };
      jobId = data.job_id;
    } catch (err) {
      push({ kind: "error", text: `Network error: ${err}` });
      setRunning(false);
      return;
    }

    const source = new EventSource(buildProxyUrl(`/api/jobs/${jobId}/output`));

    source.addEventListener("stdout", (e) => {
      push({ kind: "stdout", text: (e as MessageEvent<string>).data });
    });
    source.addEventListener("stderr", (e) => {
      push({ kind: "stderr", text: (e as MessageEvent<string>).data });
    });
    source.addEventListener("finished", (e) => {
      try {
        const payload = JSON.parse((e as MessageEvent<string>).data) as {
          exit_code: number;
          error: string;
        };
        push({
          kind: "exit",
          code: payload.exit_code,
          error: payload.error || undefined,
        });
      } catch {
        push({ kind: "exit", code: -1 });
      }
      source.close();
      setRunning(false);
    });
    source.onerror = () => {
      push({ kind: "error", text: "Output stream disconnected." });
      source.close();
      setRunning(false);
    };
  }, [input, running, tool, deviceId, push]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      execCommand();
    }
  };

  return (
    <div className="terminal-panel-inner">
      <div className="terminal-output" ref={outputRef}>
        <div style={{ display: "flex", justifyContent: "flex-end", marginBottom: 8 }}>
          <button
            className="terminal-mode-btn"
            onClick={onSwitchMode}
            disabled={running}
          >
            Interactive
          </button>
        </div>
        {entries.length === 0 && (
          <span className="terminal-hint">Type a command below and press Enter.</span>
        )}
        {entries.map((entry, i) => {
          switch (entry.kind) {
            case "command":
              return (
                <div key={i} className="terminal-line terminal-cmd">
                  <span className="terminal-prompt">{entry.tool} $</span> {entry.text}
                </div>
              );
            case "stdout":
              return (
                <div key={i} className="terminal-line terminal-stdout">
                  {entry.text}
                </div>
              );
            case "stderr":
              return (
                <div key={i} className="terminal-line terminal-stderr">
                  {entry.text}
                </div>
              );
            case "exit":
              return (
                <div key={i} className="terminal-line terminal-exit">
                  [exit {entry.code}]{entry.error ? ` ${entry.error}` : ""}
                </div>
              );
            case "error":
              return (
                <div key={i} className="terminal-line terminal-stderr">
                  {entry.text}
                </div>
              );
          }
        })}
        {running && <div className="terminal-line terminal-hint">Running...</div>}
      </div>

      <div className="terminal-input-row">
        <label className="terminal-tool-label">
          Tool:
          <input
            className="terminal-tool-input"
            value={tool}
            onChange={(e) => setTool(e.target.value)}
            disabled={running}
          />
        </label>
        <div className="terminal-cmd-input-wrap">
          <span className="terminal-prompt">{">"}</span>
          <input
            ref={inputRef}
            className="terminal-cmd-input"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={running}
            placeholder="Enter command..."
            autoFocus
          />
        </div>
      </div>
    </div>
  );
}

/* ------------------------------------------------------------------ */
/*  InteractiveTerminal – xterm.js based                               */
/* ------------------------------------------------------------------ */

type InteractiveStatus = "idle" | "running" | "finished";

function InteractiveTerminal({
  deviceId,
  onSwitchMode,
}: {
  deviceId: string;
  onSwitchMode: () => void;
}) {
  const [tool, setTool] = useState("bash");
  const [jobId, setJobId] = useState<string | null>(null);
  const [status, setStatus] = useState<InteractiveStatus>("idle");

  const containerRef = useRef<HTMLDivElement>(null);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const termRef = useRef<any>(null);
  const fitRef = useRef<{ fit(): void } | null>(null);
  const sourceRef = useRef<EventSource | null>(null);

  /* Clean up on unmount */
  useEffect(() => {
    return () => {
      sourceRef.current?.close();
      termRef.current?.dispose();
    };
  }, []);

  /* Window resize handler – re-fit terminal */
  useEffect(() => {
    if (status !== "running") return;

    const handleResize = () => {
      fitRef.current?.fit();
    };

    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, [status]);

  /* Start interactive session */
  const startSession = useCallback(async () => {
    if (status === "running") return;
    setStatus("running");

    /* Create interactive job */
    let newJobId: string;
    try {
      const res = await fetch(buildProxyUrl("/api/jobs"), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          device_id: deviceId,
          tool,
          args: [],
          timeout_ms: 0,
          interactive: true,
        }),
      });
      if (!res.ok) {
        const body = await res.text();
        setStatus("idle");
        alert(`Job creation failed (${res.status}): ${body}`);
        return;
      }
      const data = (await res.json()) as { job_id: string };
      newJobId = data.job_id;
      setJobId(newJobId);
    } catch (err) {
      setStatus("idle");
      alert(`Network error: ${err}`);
      return;
    }

    /* Dynamically import xterm.js (avoid SSR) */
    const [{ Terminal }, { FitAddon }] = await Promise.all([
      import("@xterm/xterm"),
      import("@xterm/addon-fit"),
    ]);
    // @ts-expect-error -- CSS side-effect import has no type declarations
    await import("@xterm/xterm/css/xterm.css");

    /* Dispose any previous terminal */
    termRef.current?.dispose();

    const fit = new FitAddon();
    fitRef.current = fit;

    const term = new Terminal({
      cursorBlink: true,
      theme: {
        background: "#020617",
        foreground: "#dbeafe",
        cursor: "#5eead4",
      },
      fontFamily: '"SFMono-Regular", "Menlo", "Monaco", monospace',
      fontSize: 14,
    });

    termRef.current = term;
    term.loadAddon(fit);

    if (containerRef.current) {
      containerRef.current.innerHTML = "";
      term.open(containerRef.current);
      fit.fit();

      /* Send initial size */
      fetch(buildProxyUrl(`/api/jobs/${newJobId}/resize`), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ cols: term.cols, rows: term.rows }),
      }).catch(() => {});
    }

    /* Forward keystrokes to backend */
    term.onData((data) => {
      fetch(buildProxyUrl(`/api/jobs/${newJobId}/stdin`), {
        method: "POST",
        body: data,
      }).catch(() => {});
    });

    /* Listen for resize events from FitAddon */
    term.onResize(({ cols, rows }) => {
      fetch(buildProxyUrl(`/api/jobs/${newJobId}/resize`), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ cols, rows }),
      }).catch(() => {});
    });

    /* Stream output via SSE */
    const source = new EventSource(
      buildProxyUrl(`/api/jobs/${newJobId}/output`),
    );
    sourceRef.current = source;

    source.addEventListener("stdout", (e) => {
      term.write((e as MessageEvent<string>).data);
    });
    source.addEventListener("stderr", (e) => {
      term.write((e as MessageEvent<string>).data);
    });
    source.addEventListener("finished", () => {
      term.write("\r\n[session ended]\r\n");
      source.close();
      setStatus("finished");
    });
    source.onerror = () => {
      term.write("\r\n[connection lost]\r\n");
      source.close();
      setStatus("finished");
    };
  }, [deviceId, tool, status]);

  return (
    <div className="terminal-panel-inner">
      {/* Toolbar */}
      <div className="terminal-input-row" style={{ justifyContent: "space-between" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <label className="terminal-tool-label">
            Tool:
            <input
              className="terminal-tool-input"
              value={tool}
              onChange={(e) => setTool(e.target.value)}
              disabled={status === "running"}
            />
          </label>
          {status === "idle" && (
            <button className="terminal-mode-btn" onClick={startSession}>
              Start
            </button>
          )}
          {status === "finished" && (
            <button
              className="terminal-mode-btn"
              onClick={() => {
                setJobId(null);
                setStatus("idle");
                termRef.current?.dispose();
                termRef.current = null;
              }}
            >
              New session
            </button>
          )}
        </div>
        <button
          className="terminal-mode-btn"
          onClick={onSwitchMode}
          disabled={status === "running"}
        >
          Pipe mode
        </button>
      </div>

      {/* xterm.js container */}
      <div ref={containerRef} className="xterm-container" />

      {status === "idle" && !jobId && (
        <div style={{ padding: "18px", color: "var(--muted)", fontStyle: "italic" }}>
          Select a tool and click Start to open an interactive session.
        </div>
      )}
    </div>
  );
}
