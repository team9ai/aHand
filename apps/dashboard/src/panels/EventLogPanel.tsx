import { For, Show, createMemo, createSignal, type Component } from "solid-js";
import { store, type DashEvent } from "../stores/dashboard";

function formatTime(ts: number): string {
  const d = new Date(ts);
  return d.toLocaleTimeString("en-US", {
    hour12: false,
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }) + "." + String(d.getMilliseconds()).padStart(3, "0");
}

function eventCategory(type: string): string {
  if (type.startsWith("device")) return "device";
  if (type.startsWith("job")) return "job";
  if (type.startsWith("approval")) return "approval";
  if (type.startsWith("policy")) return "policy";
  return "device";
}

function eventSummary(evt: DashEvent): string {
  switch (evt.type) {
    case "device.connected": {
      const d = evt.device as { hostname: string };
      return d?.hostname ?? "";
    }
    case "device.disconnected":
      return String(evt.deviceId ?? "").slice(0, 12);
    case "job.started":
      return `${evt.tool} ${(evt.args as string[] ?? []).join(" ")}`;
    case "job.stdout":
    case "job.stderr":
      return `${(evt.data as string ?? "").slice(0, 60)}`;
    case "job.finished":
      return `exit ${evt.exitCode}`;
    case "job.rejected":
      return String(evt.reason ?? "");
    case "approval.request":
      return `${evt.tool} - ${evt.reason}`;
    case "approval.resolved":
      return evt.approved ? "approved" : "denied";
    case "policy.state":
      return String(evt.deviceId ?? "").slice(0, 12);
    default:
      return "";
  }
}

const EventLogPanel: Component = () => {
  const [filter, setFilter] = createSignal("all");

  const filteredEvents = createMemo(() => {
    const f = filter();
    const events = store.events;
    if (f === "all") return [...events].reverse();
    return [...events].filter((e) => eventCategory(e.type) === f).reverse();
  });

  return (
    <div>
      <div class="flex gap-2 mb-2" style={{ "align-items": "center" }}>
        <h2 class="panel-title" style={{ "margin-bottom": "0" }}>
          Event Log
        </h2>
        <select
          value={filter()}
          onChange={(e) => setFilter(e.currentTarget.value)}
        >
          <option value="all">All</option>
          <option value="device">Device</option>
          <option value="job">Job</option>
          <option value="approval">Approval</option>
          <option value="policy">Policy</option>
        </select>
      </div>

      <Show
        when={filteredEvents().length > 0}
        fallback={<div class="empty-state">No events yet.</div>}
      >
        <table class="event-table">
          <thead>
            <tr>
              <th style={{ width: "100px" }}>Time</th>
              <th style={{ width: "140px" }}>Type</th>
              <th>Summary</th>
            </tr>
          </thead>
          <tbody>
            <For each={filteredEvents()}>
              {(evt) => (
                <tr>
                  <td class="mono text-muted">{formatTime(evt.ts)}</td>
                  <td>
                    <span
                      class={`event-type ${eventCategory(evt.type)}`}
                    >
                      {evt.type}
                    </span>
                  </td>
                  <td class="text-sm">{eventSummary(evt)}</td>
                </tr>
              )}
            </For>
          </tbody>
        </table>
      </Show>
    </div>
  );
};

export default EventLogPanel;
