import { createResource, createSignal, For, Show } from "solid-js";
import { api, LogsResponse } from "../lib/api";

export default function LogsPanel() {
  const [offset, setOffset] = createSignal(0);
  const limit = 50;

  const [logs] = createResource(
    offset,
    (offset) => api.getLogs(limit, offset)
  );

  function formatTimestamp(ts: number): string {
    return new Date(ts).toLocaleString();
  }

  function handleNext() {
    const current = logs();
    if (current && offset() + limit < current.total) {
      setOffset(offset() + limit);
    }
  }

  function handlePrev() {
    if (offset() >= limit) {
      setOffset(offset() - limit);
    }
  }

  return (
    <div class="panel">
      <h2>Audit Logs</h2>

      <Show when={logs.loading}>
        <p>Loading...</p>
      </Show>

      <Show when={logs.error}>
        <p class="error">Error: {logs.error.message}</p>
      </Show>

      <Show when={logs()}>
        {(data) => (
          <>
            <div class="logs-info">
              Showing {offset() + 1}-{Math.min(offset() + limit, data().total)} of {data().total}
            </div>

            <div class="table-container">
              <table class="logs-table">
                <thead>
                  <tr>
                    <th>Timestamp</th>
                    <th>Direction</th>
                    <th>Device ID</th>
                    <th>Message ID</th>
                    <th>Seq/Ack</th>
                    <th>Payload Type</th>
                  </tr>
                </thead>
                <tbody>
                  <For each={data().entries}>
                    {(entry) => (
                      <tr>
                        <td>{formatTimestamp(entry.ts_ms)}</td>
                        <td class={`direction-${entry.direction}`}>
                          {entry.direction}
                        </td>
                        <td class="device-id">{entry.device_id}</td>
                        <td class="msg-id">{entry.msg_id}</td>
                        <td>{entry.seq}/{entry.ack}</td>
                        <td class="payload-type">{entry.payload_type}</td>
                      </tr>
                    )}
                  </For>
                </tbody>
              </table>
            </div>

            <div class="pagination">
              <button
                onClick={handlePrev}
                disabled={offset() === 0}
                class="btn-secondary"
              >
                Previous
              </button>
              <button
                onClick={handleNext}
                disabled={offset() + limit >= data().total}
                class="btn-secondary"
              >
                Next
              </button>
            </div>
          </>
        )}
      </Show>
    </div>
  );
}
