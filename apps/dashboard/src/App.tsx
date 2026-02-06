import { createSignal, onMount, onCleanup, type Component } from "solid-js";
import { store, setStore, handleEvent } from "./stores/dashboard";
import { DashboardWS } from "./lib/ws";
import DevicesPanel from "./panels/DevicesPanel";
import ExecPanel from "./panels/ExecPanel";
import ApprovalsPanel from "./panels/ApprovalsPanel";
import SessionPanel from "./panels/SessionPanel";
import EventLogPanel from "./panels/EventLogPanel";
import BrowserPanel from "./panels/BrowserPanel";

type PanelId = "devices" | "exec" | "browser" | "approvals" | "sessions" | "events";

const panels: { id: PanelId; label: string }[] = [
  { id: "devices", label: "Devices" },
  { id: "exec", label: "Execute" },
  { id: "browser", label: "Browser" },
  { id: "approvals", label: "Approvals" },
  { id: "sessions", label: "Sessions" },
  { id: "events", label: "Event Log" },
];

const App: Component = () => {
  const [activePanel, setActivePanel] = createSignal<PanelId>("devices");

  onMount(() => {
    const ws = new DashboardWS();
    ws.onMessage((evt) => handleEvent(evt));
    ws.onStatus((connected) => setStore("wsConnected", connected));
    ws.connect();

    onCleanup(() => ws.close());
  });

  return (
    <div class="app">
      <header class="header">
        <h1>aHand Dashboard</h1>
        <span class="status-label">
          <span
            class={`status-dot ${store.wsConnected ? "connected" : "disconnected"}`}
          />
          {store.wsConnected ? "connected" : "disconnected"}
        </span>
      </header>

      <nav class="sidebar">
        {panels.map((p) => (
          <button
            class={`nav-btn ${activePanel() === p.id ? "active" : ""}`}
            onClick={() => setActivePanel(p.id)}
          >
            {p.label}
            {p.id === "approvals" && store.pendingApprovals.length > 0 && (
              <span class="badge">{store.pendingApprovals.length}</span>
            )}
          </button>
        ))}
      </nav>

      <main class="main">
        <div
          class={`panel ${activePanel() === "devices" ? "active" : ""}`}
        >
          <DevicesPanel />
        </div>
        <div class={`panel ${activePanel() === "exec" ? "active" : ""}`}>
          <ExecPanel />
        </div>
        <div class={`panel ${activePanel() === "browser" ? "active" : ""}`}>
          <BrowserPanel />
        </div>
        <div
          class={`panel ${activePanel() === "approvals" ? "active" : ""}`}
        >
          <ApprovalsPanel />
        </div>
        <div
          class={`panel ${activePanel() === "sessions" ? "active" : ""}`}
        >
          <SessionPanel />
        </div>
        <div
          class={`panel ${activePanel() === "events" ? "active" : ""}`}
        >
          <EventLogPanel />
        </div>
      </main>
    </div>
  );
};

export default App;
