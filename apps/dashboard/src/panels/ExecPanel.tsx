import { createSignal, For, Show, type Component } from "solid-js";
import { store } from "../stores/dashboard";
import { api } from "../lib/api";
import Terminal from "../components/Terminal";

const ExecPanel: Component = () => {
  const [tool, setTool] = createSignal("");
  const [args, setArgs] = createSignal("");
  const [cwd, setCwd] = createSignal("");
  const [deviceId, setDeviceId] = createSignal("");
  const [jobHistory, setJobHistory] = createSignal<string[]>([]);
  const [submitting, setSubmitting] = createSignal(false);

  const historyJobs = () =>
    jobHistory().flatMap((id) => {
      const j = store.jobs.find((j) => j.jobId === id);
      return j ? [j] : [];
    });

  const handleExec = async () => {
    if (!tool().trim()) return;
    setSubmitting(true);

    try {
      const res = await api.api.exec.$post({
        json: {
          tool: tool().trim(),
          args: args().trim().split(/\s+/).filter(Boolean),
          cwd: cwd().trim() || undefined,
          deviceId: deviceId() || undefined,
        },
      });
      if (res.ok) {
        const data = await res.json();
        setJobHistory((prev) => [data.jobId, ...prev]);
      }
    } catch (e) {
      console.error("exec failed:", e);
    } finally {
      setSubmitting(false);
    }
  };

  const handleCancel = async (jobId: string) => {
    try {
      await api.api.cancel.$post({
        json: { jobId, deviceId: deviceId() || undefined },
      });
    } catch (e) {
      console.error("cancel failed:", e);
    }
  };

  return (
    <div>
      <h2 class="panel-title">Execute</h2>

      <div class="card">
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
          <span class="form-label">Tool</span>
          <input
            type="text"
            placeholder="e.g. ls, curl, git"
            value={tool()}
            onInput={(e) => setTool(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && handleExec()}
          />
        </div>

        <div class="form-row">
          <span class="form-label">Args</span>
          <input
            type="text"
            placeholder="e.g. -la /tmp"
            value={args()}
            onInput={(e) => setArgs(e.currentTarget.value)}
            onKeyDown={(e) => e.key === "Enter" && handleExec()}
          />
        </div>

        <div class="form-row">
          <span class="form-label">CWD</span>
          <input
            type="text"
            placeholder="working directory (optional)"
            value={cwd()}
            onInput={(e) => setCwd(e.currentTarget.value)}
          />
        </div>

        <div class="flex gap-2 mt-2">
          <button
            class="btn btn-primary"
            onClick={handleExec}
            disabled={submitting() || !tool().trim()}
          >
            {submitting() ? "Submitting..." : "Execute"}
          </button>
        </div>
      </div>

      <For each={historyJobs()}>
        {(job) => (
          <div class="mt-3">
            <div class="card-meta mb-2">
              <span class="mono">
                {job.tool} {job.args.join(" ")}
              </span>{" "}
              <Show when={job.status === "running"}>
                <span class="text-warning">running</span>
                <button
                  class="btn btn-danger btn-sm"
                  style="margin-left: 8px"
                  onClick={() => handleCancel(job.jobId)}
                >
                  Cancel
                </button>
              </Show>
              <Show when={job.status === "pending_approval"}>
                <span class="text-warning">awaiting approval</span>
              </Show>
              <Show when={job.status === "finished" && job.error === "cancelled"}>
                <span class="text-warning">cancelled</span>
              </Show>
              <Show when={job.status === "finished" && job.error === "timeout"}>
                <span class="text-danger">timed out</span>
              </Show>
              <Show when={job.status === "finished" && job.error !== "cancelled" && job.error !== "timeout"}>
                <span
                  class={job.exitCode === 0 ? "text-success" : "text-danger"}
                >
                  exit {job.exitCode}
                </span>
              </Show>
              <Show when={job.status === "rejected"}>
                <span class="text-danger">rejected: {job.error}</span>
              </Show>
            </div>
            <Terminal stdout={job.stdout} stderr={job.stderr} />
          </div>
        )}
      </For>
    </div>
  );
};

export default ExecPanel;
