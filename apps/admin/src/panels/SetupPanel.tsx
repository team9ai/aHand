import { createSignal, Show } from "solid-js";
import { api } from "../lib/api";

type Mode = "openclaw-gateway" | "ahand-cloud";

export default function SetupPanel(props: { onComplete: () => void }) {
  const [mode, setMode] = createSignal<Mode>("openclaw-gateway");
  const [saving, setSaving] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [done, setDone] = createSignal(false);

  // ahand-cloud fields
  const [serverUrl, setServerUrl] = createSignal("");

  // openclaw-gateway fields
  const [gatewayHost, setGatewayHost] = createSignal("");
  const [gatewayPort, setGatewayPort] = createSignal(18789);
  const [nodeId, setNodeId] = createSignal("");
  const [displayName, setDisplayName] = createSignal("");
  const [authToken, setAuthToken] = createSignal("");
  const [gatewayTls, setGatewayTls] = createSignal(false);

  async function handleSave() {
    setError(null);
    setSaving(true);

    try {
      let config: any;

      if (mode() === "ahand-cloud") {
        if (!serverUrl().trim()) {
          setError("Server URL is required");
          return;
        }
        config = {
          mode: "ahand-cloud",
          server_url: serverUrl().trim(),
          default_session_mode: "trust",
        };
      } else {
        if (!gatewayHost().trim()) {
          setError("Gateway host is required");
          return;
        }
        config = {
          mode: "openclaw-gateway",
          default_session_mode: "auto_accept",
          openclaw: {
            gateway_host: gatewayHost().trim(),
            gateway_port: gatewayPort(),
            gateway_tls: gatewayTls(),
            ...(nodeId().trim() && { node_id: nodeId().trim() }),
            ...(displayName().trim() && { display_name: displayName().trim() }),
            ...(authToken().trim() && { auth_token: authToken().trim() }),
          },
        };
      }

      await api.putConfig(config);
      setDone(true);
    } catch (e: any) {
      setError(e.message || "Failed to save configuration");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div class="setup-container">
      <Show
        when={!done()}
        fallback={
          <div class="panel setup-done">
            <h2>Configuration saved</h2>
            <p>Your config has been written to <code>~/.ahand/config.toml</code></p>
            <div class="setup-next">
              <p>Next, start the daemon:</p>
              <pre>ahandd</pre>
            </div>
            <button class="btn-primary" onClick={props.onComplete}>
              Open Admin Panel
            </button>
          </div>
        }
      >
        <div class="panel">
          <h2>Welcome to aHand</h2>
          <p class="setup-subtitle">
            Configure how the daemon connects to your cloud server.
          </p>

          <div class="config-section">
            <h3>Connection Mode</h3>
            <div class="mode-select">
              <button
                class={mode() === "openclaw-gateway" ? "mode-btn active" : "mode-btn"}
                onClick={() => setMode("openclaw-gateway")}
              >
                <span class="mode-title">OpenClaw Gateway</span>
                <span class="mode-desc">Connect via OpenClaw Gateway node-host</span>
              </button>
              <button
                class={mode() === "ahand-cloud" ? "mode-btn active" : "mode-btn"}
                onClick={() => setMode("ahand-cloud")}
              >
                <span class="mode-title">AHand Cloud</span>
                <span class="mode-desc">Direct WebSocket to aHand cloud server</span>
              </button>
            </div>
          </div>

          <Show when={mode() === "ahand-cloud"}>
            <div class="config-section">
              <h3>Server</h3>
              <div class="form-grid">
                <div class="form-field form-field-full">
                  <label>WebSocket URL</label>
                  <input
                    type="text"
                    placeholder="ws://your-server.com/ws"
                    value={serverUrl()}
                    onInput={(e) => setServerUrl(e.currentTarget.value)}
                  />
                </div>
              </div>
            </div>
          </Show>

          <Show when={mode() === "openclaw-gateway"}>
            <div class="config-section">
              <h3>Gateway</h3>
              <div class="form-grid">
                <div class="form-field">
                  <label>Host</label>
                  <input
                    type="text"
                    placeholder="gateway.example.com"
                    value={gatewayHost()}
                    onInput={(e) => setGatewayHost(e.currentTarget.value)}
                  />
                </div>
                <div class="form-field">
                  <label>Port</label>
                  <input
                    type="number"
                    value={gatewayPort()}
                    onInput={(e) => setGatewayPort(parseInt(e.currentTarget.value) || 18789)}
                  />
                </div>
                <div class="form-field form-field-checkbox">
                  <label>
                    <input
                      type="checkbox"
                      checked={gatewayTls()}
                      onChange={(e) => setGatewayTls(e.currentTarget.checked)}
                    />
                    Use TLS
                  </label>
                </div>
              </div>
            </div>

            <div class="config-section">
              <h3>Authentication</h3>
              <div class="form-grid">
                <div class="form-field">
                  <label>Node ID</label>
                  <input
                    type="text"
                    placeholder="my-node-01"
                    value={nodeId()}
                    onInput={(e) => setNodeId(e.currentTarget.value)}
                  />
                </div>
                <div class="form-field">
                  <label>Display Name</label>
                  <input
                    type="text"
                    placeholder="My Local Machine"
                    value={displayName()}
                    onInput={(e) => setDisplayName(e.currentTarget.value)}
                  />
                </div>
                <div class="form-field form-field-full">
                  <label>Auth Token</label>
                  <input
                    type="password"
                    placeholder="Token from gateway admin"
                    value={authToken()}
                    onInput={(e) => setAuthToken(e.currentTarget.value)}
                  />
                </div>
              </div>
            </div>
          </Show>

          <Show when={error()}>
            <div class="error-message">{error()}</div>
          </Show>

          <div class="setup-actions">
            <button
              class="btn-primary"
              onClick={handleSave}
              disabled={saving()}
            >
              {saving() ? "Saving..." : "Save & Continue"}
            </button>
          </div>
        </div>
      </Show>
    </div>
  );
}
