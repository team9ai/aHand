import { createSignal, For, Show, type Component } from "solid-js";
import { store } from "../stores/dashboard";
import { api } from "../lib/api";

const ApprovalsPanel: Component = () => {
  const [refusalReasons, setRefusalReasons] = createSignal<Record<string, string>>({});
  const [showReasonInput, setShowReasonInput] = createSignal<Record<string, boolean>>({});

  const handleApprove = async (jobId: string) => {
    try {
      await api.api.approve.$post({
        json: { jobId, approved: true },
      });
    } catch (e) {
      console.error("approve failed:", e);
    }
  };

  const handleRefuse = async (jobId: string) => {
    try {
      await api.api.approve.$post({
        json: { jobId, approved: false },
      });
    } catch (e) {
      console.error("refuse failed:", e);
    }
  };

  const handleRefuseWithReason = async (jobId: string) => {
    const reason = refusalReasons()[jobId] ?? "";
    try {
      await api.api.approve.$post({
        json: { jobId, approved: false, reason },
      });
      // Clear state.
      setRefusalReasons((r) => {
        const next = { ...r };
        delete next[jobId];
        return next;
      });
      setShowReasonInput((r) => {
        const next = { ...r };
        delete next[jobId];
        return next;
      });
    } catch (e) {
      console.error("refuse with reason failed:", e);
    }
  };

  const toggleReasonInput = (jobId: string) => {
    setShowReasonInput((r) => ({ ...r, [jobId]: !r[jobId] }));
  };

  const timeAgo = (ms: number) => {
    const diff = Date.now() - ms;
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return "just now";
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    return `${hours}h ${mins % 60}m ago`;
  };

  return (
    <div>
      <h2 class="panel-title">Pending Approvals</h2>
      <Show
        when={store.pendingApprovals.length > 0}
        fallback={<div class="empty-state">No pending approvals.</div>}
      >
        <For each={store.pendingApprovals}>
          {(approval) => {
            const timeLeft = () => {
              const remaining = approval.expiresMs - Date.now();
              if (remaining <= 0) return "expired";
              const h = Math.floor(remaining / 3600000);
              const m = Math.floor((remaining % 3600000) / 60000);
              return h > 0 ? `${h}h ${m}m` : `${m}m`;
            };

            return (
              <div class="card">
                <div class="card-title mono">
                  {approval.tool} {approval.args.join(" ")}
                </div>
                <div class="card-meta">
                  <div>Reason: {approval.reason}</div>
                  <div>Caller: {approval.callerUid}</div>
                  <Show when={approval.cwd}>
                    <div>CWD: {approval.cwd}</div>
                  </Show>
                  <Show when={approval.detectedDomains.length > 0}>
                    <div class="mt-2">
                      Domains:{" "}
                      <For each={approval.detectedDomains}>
                        {(d) => <span class="tag">{d}</span>}
                      </For>
                    </div>
                  </Show>
                  <div class="text-muted mt-2">Expires in: {timeLeft()}</div>
                </div>

                {/* Previous refusals */}
                <Show when={approval.previousRefusals.length > 0}>
                  <div class="mt-3">
                    <For each={approval.previousRefusals}>
                      {(r) => (
                        <div class="refusal-hint">
                          Previous refusal: "{r.reason}" ({timeAgo(r.refusedAtMs)})
                        </div>
                      )}
                    </For>
                  </div>
                </Show>

                <div class="flex gap-2 mt-3" style="flex-wrap: wrap">
                  <button
                    class="btn btn-success btn-sm"
                    onClick={() => handleApprove(approval.jobId)}
                  >
                    Approve
                  </button>
                  <button
                    class="btn btn-danger btn-sm"
                    onClick={() => handleRefuse(approval.jobId)}
                  >
                    Refuse
                  </button>
                  <button
                    class="btn btn-sm"
                    classList={{ "btn-warning": true }}
                    onClick={() => toggleReasonInput(approval.jobId)}
                  >
                    Refuse with Reason
                  </button>
                </div>

                <Show when={showReasonInput()[approval.jobId]}>
                  <div class="flex gap-2 mt-2">
                    <input
                      type="text"
                      placeholder="Enter refusal reason..."
                      value={refusalReasons()[approval.jobId] ?? ""}
                      onInput={(e) =>
                        setRefusalReasons((r) => ({
                          ...r,
                          [approval.jobId]: e.currentTarget.value,
                        }))
                      }
                      onKeyDown={(e) => {
                        if (e.key === "Enter") handleRefuseWithReason(approval.jobId);
                      }}
                      style="flex: 1"
                    />
                    <button
                      class="btn btn-danger btn-sm"
                      onClick={() => handleRefuseWithReason(approval.jobId)}
                      disabled={!(refusalReasons()[approval.jobId] ?? "").trim()}
                    >
                      Send
                    </button>
                  </div>
                </Show>
              </div>
            );
          }}
        </For>
      </Show>
    </div>
  );
};

export default ApprovalsPanel;
