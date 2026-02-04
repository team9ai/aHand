import { createSignal, For, Show, type Component } from "solid-js";
import { store } from "../stores/dashboard";
import { api } from "../lib/api";
import TagList from "../components/TagList";
import type { InferRequestType } from "hono";

type updatePolicyT = InferRequestType<
  (typeof api.api.policy)[":deviceId"]["$post"]
>["json"];

const PolicyPanel: Component = () => {
  const [selectedDevice, setSelectedDevice] = createSignal("");

  const activeDeviceId = () =>
    selectedDevice() || store.devices[0]?.deviceId || "";

  const policy = () => store.policyByDevice[activeDeviceId()];

  const refreshPolicy = async () => {
    const id = activeDeviceId();
    if (!id) return;
    try {
      await api.api.policy[":deviceId"].$get({
        param: { deviceId: id },
      });
    } catch (e) {
      console.error("policy query failed:", e);
    }
  };

  const updatePolicy = async (update: updatePolicyT) => {
    const id = activeDeviceId();
    if (!id) return;
    try {
      await api.api.policy[":deviceId"].$post({
        param: { deviceId: id },
        json: update,
      });
    } catch (e) {
      console.error("policy update failed:", e);
    }
  };

  return (
    <div>
      <h2 class="panel-title">Policy</h2>

      <Show
        when={store.devices.length > 0}
        fallback={
          <div class="empty-state">
            No devices connected. Connect a device to manage its policy.
          </div>
        }
      >
        <div class="flex gap-2 mb-2">
          <select
            value={selectedDevice()}
            onChange={(e) => setSelectedDevice(e.currentTarget.value)}
          >
            <For each={store.devices}>
              {(d) => <option value={d.deviceId}>{d.hostname}</option>}
            </For>
          </select>
          <button class="btn btn-sm" onClick={refreshPolicy}>
            Refresh
          </button>
        </div>

        <Show
          when={policy()}
          fallback={
            <div class="card">
              <div class="text-muted">
                No policy data yet. Click Refresh to query.
              </div>
            </div>
          }
        >
          {(p) => (
            <div>
              <div class="card">
                <div class="card-title">Allowed Tools</div>
                <TagList
                  items={p().allowedTools}
                  onAdd={(t) => updatePolicy({ addAllowedTools: [t] })}
                  onRemove={(t) => updatePolicy({ removeAllowedTools: [t] })}
                  placeholder="Add allowed tool..."
                />
              </div>

              <div class="card">
                <div class="card-title">Denied Tools</div>
                <TagList
                  items={p().deniedTools}
                  onAdd={(t) => updatePolicy({ addDeniedTools: [t] })}
                  onRemove={(t) => updatePolicy({ removeDeniedTools: [t] })}
                  placeholder="Add denied tool..."
                />
              </div>

              <div class="card">
                <div class="card-title">Allowed Domains</div>
                <TagList
                  items={p().allowedDomains}
                  onAdd={(d) => updatePolicy({ addAllowedDomains: [d] })}
                  onRemove={(d) => updatePolicy({ removeAllowedDomains: [d] })}
                  placeholder="Add allowed domain..."
                />
              </div>

              <div class="card">
                <div class="card-title">Denied Paths</div>
                <TagList
                  items={p().deniedPaths}
                  onAdd={(d) => updatePolicy({ addDeniedPaths: [d] })}
                  onRemove={(d) => updatePolicy({ removeDeniedPaths: [d] })}
                  placeholder="Add denied path..."
                />
              </div>

              <div class="card">
                <div class="card-title">Approval Timeout</div>
                <div class="text-sm text-muted">
                  {p().approvalTimeoutSecs}s (
                  {Math.floor(p().approvalTimeoutSecs / 3600)}h)
                </div>
              </div>
            </div>
          )}
        </Show>
      </Show>
    </div>
  );
};

export default PolicyPanel;
