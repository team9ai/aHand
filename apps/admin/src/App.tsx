import { createSignal, Show } from "solid-js";
import { getToken } from "./lib/api";
import StatusPanel from "./panels/StatusPanel";
import ConfigPanel from "./panels/ConfigPanel";
import LogsPanel from "./panels/LogsPanel";
import RunsPanel from "./panels/RunsPanel";

type Tab = "status" | "config" | "logs" | "runs";

export default function App() {
  const [activeTab, setActiveTab] = createSignal<Tab>("status");
  const token = getToken();

  return (
    <Show
      when={token}
      fallback={
        <div class="container">
          <div class="error-box">
            <h1>Authentication Required</h1>
            <p>
              No authentication token found. Please start the admin panel with:
            </p>
            <pre>ahandctl configure</pre>
          </div>
        </div>
      }
    >
      <div class="container">
        <header class="header">
          <h1>aHand Admin Panel</h1>
          <div class="tabs">
            <button
              class={activeTab() === "status" ? "tab active" : "tab"}
              onClick={() => setActiveTab("status")}
            >
              Status
            </button>
            <button
              class={activeTab() === "config" ? "tab active" : "tab"}
              onClick={() => setActiveTab("config")}
            >
              Config
            </button>
            <button
              class={activeTab() === "logs" ? "tab active" : "tab"}
              onClick={() => setActiveTab("logs")}
            >
              Logs
            </button>
            <button
              class={activeTab() === "runs" ? "tab active" : "tab"}
              onClick={() => setActiveTab("runs")}
            >
              Runs
            </button>
          </div>
        </header>

        <main class="content">
          <Show when={activeTab() === "status"}>
            <StatusPanel />
          </Show>
          <Show when={activeTab() === "config"}>
            <ConfigPanel />
          </Show>
          <Show when={activeTab() === "logs"}>
            <LogsPanel />
          </Show>
          <Show when={activeTab() === "runs"}>
            <RunsPanel />
          </Show>
        </main>
      </div>
    </Show>
  );
}
