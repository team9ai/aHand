"use client";

import { useJobOutput } from "@/hooks/use-job-output";

export function JobOutputViewer({ jobId }: { jobId: string }) {
  const { entries, status, error } = useJobOutput(jobId);

  return (
    <section className="terminal-panel" aria-label="Job output viewer">
      <div className="terminal-toolbar">
        <span>Terminal Output</span>
        <span className="terminal-status" data-status={status}>
          {status}
        </span>
      </div>
      <pre className="terminal-output">
        {entries.length > 0 ? entries.map((entry, index) => <div key={`${jobId}-${index}`}>{entry.text}</div>) : "Awaiting output..."}
      </pre>
      {error ? (
        <p className="inline-error" role="alert">
          {error}
        </p>
      ) : null}
    </section>
  );
}
