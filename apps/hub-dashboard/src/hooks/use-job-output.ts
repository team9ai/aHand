"use client";

import { useEffect, useState } from "react";
import { buildProxyUrl } from "@/lib/hub-paths";

export type JobOutputEntry = {
  type: "stdout" | "stderr" | "progress" | "finished";
  text: string;
};

type UseJobOutputResult = {
  entries: JobOutputEntry[];
  status: "idle" | "streaming" | "complete" | "error";
  error: string | null;
};

export function useJobOutput(jobId: string): UseJobOutputResult {
  const [entries, setEntries] = useState<JobOutputEntry[]>([]);
  const [status, setStatus] = useState<UseJobOutputResult["status"]>("streaming");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const source = new EventSource(buildProxyUrl(`/api/jobs/${jobId}/output`));

    const pushEntry = (type: JobOutputEntry["type"], text: string) => {
      setEntries((current) => [...current, { type, text }]);
    };

    source.addEventListener("stdout", (event) => {
      pushEntry("stdout", (event as MessageEvent<string>).data);
    });
    source.addEventListener("stderr", (event) => {
      pushEntry("stderr", (event as MessageEvent<string>).data);
    });
    source.addEventListener("progress", (event) => {
      pushEntry("progress", `Progress ${(event as MessageEvent<string>).data}%`);
    });
    source.addEventListener("finished", (event) => {
      try {
        const payload = JSON.parse((event as MessageEvent<string>).data) as {
          exit_code: number;
          error: string;
        };
        const text = payload.error
          ? `Command ended with error: ${payload.error}`
          : `Command exited with code ${payload.exit_code}`;
        pushEntry("finished", text);
      } catch {
        pushEntry("finished", "Command finished");
      }
      setStatus("complete");
      source.close();
    });

    source.onerror = () => {
      setStatus((current) => (current === "complete" ? current : "error"));
      setError("Live output connection lost.");
      source.close();
    };

    return () => {
      source.close();
    };
  }, [jobId]);

  return { entries, status, error };
}
