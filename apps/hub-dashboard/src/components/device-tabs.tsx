"use client";

import { useState } from "react";
import { JobRecord } from "@/lib/api";
import { DeviceTerminal } from "./device-terminal";
import { DeviceBrowser } from "./device-browser";

type Job = JobRecord;

export function DeviceTabs({
  deviceId,
  jobs,
  online,
  capabilities,
}: {
  deviceId: string;
  jobs: Job[];
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
          {jobs.length === 0 ? (
            <p className="empty-state">No jobs found for this device.</p>
          ) : (
            <table className="data-table">
              <thead>
                <tr>
                  <th>Job ID</th>
                  <th>Tool</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {jobs.map((job) => (
                  <tr key={job.id}>
                    <td className="table-subtle">{job.id}</td>
                    <td>{job.tool}</td>
                    <td>{job.status}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
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
