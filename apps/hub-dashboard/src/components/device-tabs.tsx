"use client";

import { useState } from "react";
import { DeviceJobsPanel } from "./device-jobs-panel";
import { DeviceTerminal } from "./device-terminal";
import { DeviceBrowser } from "./device-browser";

export function DeviceTabs({
  deviceId,
  online,
  capabilities,
}: {
  deviceId: string;
  online: boolean;
  capabilities: string[];
}) {
  const hasBrowser = online && capabilities.includes("browser");
  const [tab, setTab] = useState<"jobs" | "terminal" | "browser">(
    hasBrowser ? "browser" : online ? "terminal" : "jobs",
  );

  return (
    <article className="surface-panel device-tabs-panel">
      <div className="device-tabs-header">
        <button
          className={`device-tab ${tab === "jobs" ? "device-tab-active" : ""}`}
          onClick={() => setTab("jobs")}
        >
          Jobs
        </button>
        {online && (
          <button
            className={`device-tab ${tab === "terminal" ? "device-tab-active" : ""}`}
            onClick={() => setTab("terminal")}
          >
            Terminal
          </button>
        )}
        {hasBrowser && (
          <button
            className={`device-tab ${tab === "browser" ? "device-tab-active" : ""}`}
            onClick={() => setTab("browser")}
          >
            Browser
          </button>
        )}
      </div>

      {tab === "jobs" && (
        <div className="device-tab-content">
          <DeviceJobsPanel deviceId={deviceId} />
        </div>
      )}

      {tab === "terminal" && online && (
        <DeviceTerminal deviceId={deviceId} />
      )}

      {tab === "browser" && hasBrowser && (
        <DeviceBrowser deviceId={deviceId} />
      )}
    </article>
  );
}
