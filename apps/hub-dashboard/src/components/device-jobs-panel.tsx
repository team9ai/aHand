"use client";

import { useCallback, useEffect, useState } from "react";
import Link from "next/link";
import { buildProxyUrl } from "@/lib/hub-paths";

type Job = {
  id: string;
  tool: string;
  args: string[];
  status: string;
  exit_code?: number | null;
  error?: string | null;
  created_at?: string;
  started_at?: string | null;
  finished_at?: string | null;
};

const ACTIVE_STATUSES = new Set(["pending", "sent", "running"]);
const POLL_INTERVAL_MS = 3000;
const RECENT_LIMIT = 10;

export function DeviceJobsPanel({ deviceId }: { deviceId: string }) {
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [cancelling, setCancelling] = useState<Set<string>>(new Set());

  const fetchJobs = useCallback(async () => {
    try {
      const res = await fetch(
        buildProxyUrl(`/api/jobs?device_id=${encodeURIComponent(deviceId)}`),
        { cache: "no-store" },
      );
      if (!res.ok) {
        setError(`Failed to load jobs (${res.status})`);
        return;
      }
      const data = (await res.json()) as Job[];
      setJobs(data);
      setError(null);
    } catch (err) {
      setError(`Network error: ${err}`);
    }
  }, [deviceId]);

  useEffect(() => {
    fetchJobs();
    const timer = setInterval(fetchJobs, POLL_INTERVAL_MS);
    return () => clearInterval(timer);
  }, [fetchJobs]);

  const cancelJob = useCallback(
    async (jobId: string) => {
      if (!confirm(`Cancel job ${jobId}?`)) return;
      setCancelling((prev) => new Set(prev).add(jobId));
      try {
        const res = await fetch(
          buildProxyUrl(`/api/jobs/${encodeURIComponent(jobId)}/cancel`),
          { method: "POST" },
        );
        if (!res.ok) {
          const body = await res.text();
          alert(`Cancel failed (${res.status}): ${body}`);
        }
      } catch (err) {
        alert(`Network error: ${err}`);
      } finally {
        setCancelling((prev) => {
          const next = new Set(prev);
          next.delete(jobId);
          return next;
        });
        fetchJobs();
      }
    },
    [fetchJobs],
  );

  if (jobs === null) {
    return <p className="empty-state">Loading jobs…</p>;
  }

  const active = jobs
    .filter((j) => ACTIVE_STATUSES.has(j.status.toLowerCase()))
    .sort((a, b) => (b.created_at ?? "").localeCompare(a.created_at ?? ""));
  const recent = jobs
    .filter((j) => !ACTIVE_STATUSES.has(j.status.toLowerCase()))
    .sort((a, b) =>
      (b.finished_at ?? b.created_at ?? "").localeCompare(
        a.finished_at ?? a.created_at ?? "",
      ),
    )
    .slice(0, RECENT_LIMIT);

  return (
    <div className="device-jobs-panel">
      {error && <p className="inline-error">{error}</p>}

      <section className="jobs-section">
        <h3 className="jobs-section-title">
          Active
          <span className="jobs-section-count">{active.length}</span>
        </h3>
        {active.length > 0 ? (
          <ul className="activity-list">
            {active.map((job) => (
              <li className="activity-row" key={job.id}>
                <div className="job-info">
                  <Link className="table-link" href={`/jobs/${job.id}`}>
                    {job.tool}
                  </Link>
                  <p className="dashboard-copy">{job.args.join(" ")}</p>
                </div>
                <div className="job-actions">
                  <span className={`status-pill status-${job.status.toLowerCase()}`}>
                    {job.status.toLowerCase()}
                  </span>
                  <button
                    className="terminal-mode-btn job-cancel-btn"
                    onClick={() => cancelJob(job.id)}
                    disabled={cancelling.has(job.id)}
                  >
                    {cancelling.has(job.id) ? "Cancelling…" : "Cancel"}
                  </button>
                </div>
              </li>
            ))}
          </ul>
        ) : (
          <p className="empty-state">No active jobs.</p>
        )}
      </section>

      <section className="jobs-section">
        <h3 className="jobs-section-title">
          Recent
          <span className="jobs-section-count">{recent.length}</span>
        </h3>
        {recent.length > 0 ? (
          <ul className="activity-list">
            {recent.map((job) => (
              <li className="activity-row" key={job.id}>
                <div className="job-info">
                  <Link className="table-link" href={`/jobs/${job.id}`}>
                    {job.tool}
                  </Link>
                  <p className="dashboard-copy">{job.args.join(" ")}</p>
                </div>
                <span className={`status-pill status-${job.status.toLowerCase()}`}>
                  {job.status.toLowerCase()}
                  {job.exit_code != null && ` · exit ${job.exit_code}`}
                </span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="empty-state">No recent jobs.</p>
        )}
      </section>
    </div>
  );
}
