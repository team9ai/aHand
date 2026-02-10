import { createResource, createSignal, Show } from "solid-js";
import { api, getToken } from "../lib/api";

interface ConfigData {
  mode?: string;
  server_url?: string;
  device_id?: string;
  max_concurrent_jobs?: number;
  data_dir?: string;
  debug_ipc?: boolean;
  ipc_socket_path?: string;
  ipc_socket_mode?: number;
  trust_timeout_mins?: number;
  default_session_mode?: string;
  policy?: {
    allowed_tools?: string[];
    denied_tools?: string[];
    denied_paths?: string[];
    allowed_domains?: string[];
    approval_timeout_secs?: number;
  };
  browser?: {
    enabled?: boolean;
    binary_path?: string;
    executable_path?: string;
    home_dir?: string;
    socket_dir?: string;
    browsers_path?: string;
    default_timeout_ms?: number;
    max_sessions?: number;
    allowed_domains?: string[];
    denied_domains?: string[];
    downloads_dir?: string;
    headed?: boolean;
  };
  openclaw?: {
    gateway_host?: string;
    gateway_port?: number;
    gateway_tls?: boolean;
    gateway_tls_fingerprint?: string;
    node_id?: string;
    display_name?: string;
    auth_token?: string;
    auth_password?: string;
    exec_approvals_path?: string;
  };
}

export default function ConfigPanel() {
  const [config, { refetch }] = createResource<ConfigData>(api.getConfig);
  const [editMode, setEditMode] = createSignal<"form" | "json">("form");
  const [viewMode, setViewMode] = createSignal<"simple" | "advanced">("simple");
  const [jsonValue, setJsonValue] = createSignal("");
  const [saving, setSaving] = createSignal(false);
  const [saveError, setSaveError] = createSignal<string | null>(null);
  const [saveSuccess, setSaveSuccess] = createSignal(false);

  // Browser init state
  const [browserInitLog, setBrowserInitLog] = createSignal("");
  const [browserInitRunning, setBrowserInitRunning] = createSignal(false);

  // Form state
  const [formData, setFormData] = createSignal<ConfigData>({});

  // Initialize form data when config loads
  const initForm = () => {
    const data = config();
    if (data) {
      setFormData({
        ...data,
        policy: data.policy || {},
        browser: data.browser || {},
        openclaw: data.openclaw || {},
      });
    }
  };

  function handleEditJson() {
    const current = config();
    if (current) {
      setJsonValue(JSON.stringify(current, null, 2));
      setEditMode("json");
    }
  }

  function handleEditForm() {
    initForm();
    setEditMode("form");
  }

  async function handleSaveForm() {
    setSaving(true);
    setSaveError(null);
    setSaveSuccess(false);

    try {
      const data = { ...formData() };

      // In simple mode, force openclaw-gateway mode
      if (viewMode() === "simple") {
        data.mode = "openclaw-gateway";
        if (!data.default_session_mode) {
          data.default_session_mode = "auto_accept";
        }
      }

      // Remove empty optional sections
      const cleaned: ConfigData = { ...data };
      if (cleaned.browser && Object.keys(cleaned.browser).length === 0) {
        delete cleaned.browser;
      }
      if (cleaned.openclaw && Object.keys(cleaned.openclaw).length === 0) {
        delete cleaned.openclaw;
      }

      await api.putConfig(cleaned);
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 3000);
      refetch();
    } catch (e: any) {
      setSaveError(e.message || "Failed to save config");
    } finally {
      setSaving(false);
    }
  }

  async function handleSaveJson() {
    setSaving(true);
    setSaveError(null);
    setSaveSuccess(false);

    try {
      const parsed = JSON.parse(jsonValue());
      await api.putConfig(parsed);
      setEditMode("form");
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 3000);
      refetch();
    } catch (e: any) {
      setSaveError(e.message || "Failed to save config");
    } finally {
      setSaving(false);
    }
  }

  function updateField(field: keyof ConfigData, value: any) {
    setFormData((prev) => ({ ...prev, [field]: value }));
  }

  function updateNestedField(
    section: "policy" | "browser" | "openclaw",
    field: string,
    value: any
  ) {
    setFormData((prev) => ({
      ...prev,
      [section]: { ...(prev[section] || {}), [field]: value },
    }));
  }

  function updateArrayField(
    section: "policy" | "browser",
    field: string,
    value: string
  ) {
    const arr = value
      .split("\n")
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
    updateNestedField(section, field, arr);
  }

  function handleBrowserInit() {
    setBrowserInitLog("");
    setBrowserInitRunning(true);

    const token = getToken();
    const url = `/api/browser/init?token=${token}`;
    const eventSource = new EventSource(url);

    eventSource.onmessage = (event) => {
      const data = JSON.parse(event.data);
      if (data.line) {
        setBrowserInitLog((prev) => prev + data.line + "\n");
      }
      if (data.status === "done") {
        setBrowserInitRunning(false);
        eventSource.close();
      }
    };

    eventSource.onerror = () => {
      setBrowserInitRunning(false);
      eventSource.close();
    };
  }

  // ── Simple Mode Form ────────────────────────────────────────────

  function SimpleForm() {
    return (
      <div class="config-form" onMount={initForm}>
        <section class="config-section">
          <h3>OpenClaw Gateway</h3>
          <div class="form-grid">
            <div class="form-field">
              <label>Gateway Host</label>
              <input
                type="text"
                value={formData().openclaw?.gateway_host || ""}
                onInput={(e) =>
                  updateNestedField("openclaw", "gateway_host", e.currentTarget.value)
                }
                placeholder="127.0.0.1"
              />
            </div>

            <div class="form-field">
              <label>Gateway Port</label>
              <input
                type="number"
                value={formData().openclaw?.gateway_port || 18789}
                onInput={(e) =>
                  updateNestedField(
                    "openclaw",
                    "gateway_port",
                    parseInt(e.currentTarget.value)
                  )
                }
              />
            </div>

            <div class="form-field">
              <label>Display Name</label>
              <input
                type="text"
                value={formData().openclaw?.display_name || ""}
                onInput={(e) =>
                  updateNestedField("openclaw", "display_name", e.currentTarget.value)
                }
                placeholder="My Node"
              />
            </div>

            <div class="form-field">
              <label>Auth Token</label>
              <input
                type="password"
                value={formData().openclaw?.auth_token || ""}
                onInput={(e) =>
                  updateNestedField("openclaw", "auth_token", e.currentTarget.value)
                }
              />
            </div>

            <div class="form-field form-field-checkbox">
              <label>
                <input
                  type="checkbox"
                  checked={formData().openclaw?.gateway_tls || false}
                  onChange={(e) =>
                    updateNestedField("openclaw", "gateway_tls", e.currentTarget.checked)
                  }
                />
                Use TLS
              </label>
            </div>

            <div class="form-field">
              <label>Default Session Mode</label>
              <select
                value={formData().default_session_mode || "auto_accept"}
                onChange={(e) => updateField("default_session_mode", e.currentTarget.value)}
              >
                <option value="auto_accept">Auto Accept (Trust All)</option>
                <option value="strict">Strict (Require Approval)</option>
                <option value="inactive">Inactive (Deny All)</option>
              </select>
            </div>
          </div>
        </section>

        <div class="view-toggle">
          <button class="toggle-link" onClick={() => setViewMode("advanced")}>
            Show Advanced Settings
          </button>
        </div>
      </div>
    );
  }

  // ── Advanced Mode Form ──────────────────────────────────────────

  function AdvancedForm() {
    return (
      <div class="config-form" onMount={initForm}>
        <div class="view-toggle">
          <button class="toggle-link" onClick={() => setViewMode("simple")}>
            Back to Simple Mode
          </button>
        </div>

        {/* General Section */}
        <section class="config-section">
          <h3>General</h3>
          <div class="form-grid">
            <div class="form-field">
              <label>Connection Mode</label>
              <select
                value={formData().mode || "ahand-cloud"}
                onChange={(e) => updateField("mode", e.currentTarget.value)}
              >
                <option value="ahand-cloud">aHand Cloud</option>
                <option value="openclaw-gateway">OpenClaw Gateway</option>
              </select>
            </div>

            <div class="form-field">
              <label>Server URL</label>
              <input
                type="text"
                value={formData().server_url || ""}
                onInput={(e) => updateField("server_url", e.currentTarget.value)}
                placeholder="ws://localhost:3000/ws"
              />
            </div>

            <div class="form-field">
              <label>Device ID</label>
              <input
                type="text"
                value={formData().device_id || ""}
                onInput={(e) => updateField("device_id", e.currentTarget.value)}
                placeholder="Auto-generated"
              />
            </div>

            <div class="form-field">
              <label>Max Concurrent Jobs</label>
              <input
                type="number"
                value={formData().max_concurrent_jobs || 8}
                onInput={(e) =>
                  updateField("max_concurrent_jobs", parseInt(e.currentTarget.value))
                }
              />
            </div>

            <div class="form-field">
              <label>Data Directory</label>
              <input
                type="text"
                value={formData().data_dir || ""}
                onInput={(e) => updateField("data_dir", e.currentTarget.value)}
                placeholder="~/.ahand/data"
              />
            </div>

            <div class="form-field">
              <label>Trust Timeout (minutes)</label>
              <input
                type="number"
                value={formData().trust_timeout_mins || 60}
                onInput={(e) =>
                  updateField("trust_timeout_mins", parseInt(e.currentTarget.value))
                }
              />
            </div>

            <div class="form-field">
              <label>Default Session Mode</label>
              <select
                value={formData().default_session_mode || "inactive"}
                onChange={(e) => updateField("default_session_mode", e.currentTarget.value)}
              >
                <option value="auto_accept">Auto Accept (Trust All)</option>
                <option value="trust">Trust (With Timeout)</option>
                <option value="strict">Strict (Require Approval)</option>
                <option value="inactive">Inactive (Deny All)</option>
              </select>
            </div>

            <div class="form-field form-field-checkbox">
              <label>
                <input
                  type="checkbox"
                  checked={formData().debug_ipc || false}
                  onChange={(e) => updateField("debug_ipc", e.currentTarget.checked)}
                />
                Enable Debug IPC
              </label>
            </div>
          </div>
        </section>

        {/* Policy Section */}
        <section class="config-section">
          <h3>Policy</h3>
          <div class="form-grid">
            <div class="form-field">
              <label>Approval Timeout (seconds)</label>
              <input
                type="number"
                value={formData().policy?.approval_timeout_secs || 86400}
                onInput={(e) =>
                  updateNestedField(
                    "policy",
                    "approval_timeout_secs",
                    parseInt(e.currentTarget.value)
                  )
                }
              />
            </div>

            <div class="form-field form-field-full">
              <label>Allowed Tools (one per line)</label>
              <textarea
                rows={4}
                value={(formData().policy?.allowed_tools || []).join("\n")}
                onInput={(e) =>
                  updateArrayField("policy", "allowed_tools", e.currentTarget.value)
                }
                placeholder="bash&#10;grep&#10;..."
              />
            </div>

            <div class="form-field form-field-full">
              <label>Denied Tools (one per line)</label>
              <textarea
                rows={4}
                value={(formData().policy?.denied_tools || []).join("\n")}
                onInput={(e) =>
                  updateArrayField("policy", "denied_tools", e.currentTarget.value)
                }
              />
            </div>

            <div class="form-field form-field-full">
              <label>Denied Paths (one per line)</label>
              <textarea
                rows={4}
                value={(formData().policy?.denied_paths || []).join("\n")}
                onInput={(e) =>
                  updateArrayField("policy", "denied_paths", e.currentTarget.value)
                }
                placeholder="/etc&#10;/root&#10;..."
              />
            </div>

            <div class="form-field form-field-full">
              <label>Allowed Domains (one per line)</label>
              <textarea
                rows={4}
                value={(formData().policy?.allowed_domains || []).join("\n")}
                onInput={(e) =>
                  updateArrayField("policy", "allowed_domains", e.currentTarget.value)
                }
                placeholder="github.com&#10;*.example.com&#10;..."
              />
            </div>
          </div>
        </section>

        {/* Browser Section */}
        <section class="config-section">
          <h3>Browser</h3>
          <div class="form-grid">
            <div class="form-field form-field-checkbox">
              <label>
                <input
                  type="checkbox"
                  checked={formData().browser?.enabled || false}
                  onChange={(e) =>
                    updateNestedField("browser", "enabled", e.currentTarget.checked)
                  }
                />
                Enable Browser Capabilities
              </label>
            </div>

            <div class="form-field">
              <label>Binary Path</label>
              <input
                type="text"
                value={formData().browser?.binary_path || ""}
                onInput={(e) =>
                  updateNestedField("browser", "binary_path", e.currentTarget.value)
                }
                placeholder="~/.ahand/bin/agent-browser"
              />
            </div>

            <div class="form-field">
              <label>Max Sessions</label>
              <input
                type="number"
                value={formData().browser?.max_sessions || 4}
                onInput={(e) =>
                  updateNestedField(
                    "browser",
                    "max_sessions",
                    parseInt(e.currentTarget.value)
                  )
                }
              />
            </div>

            <div class="form-field">
              <label>Default Timeout (ms)</label>
              <input
                type="number"
                value={formData().browser?.default_timeout_ms || 30000}
                onInput={(e) =>
                  updateNestedField(
                    "browser",
                    "default_timeout_ms",
                    parseInt(e.currentTarget.value)
                  )
                }
              />
            </div>

            <div class="form-field form-field-checkbox">
              <label>
                <input
                  type="checkbox"
                  checked={formData().browser?.headed || false}
                  onChange={(e) =>
                    updateNestedField("browser", "headed", e.currentTarget.checked)
                  }
                />
                Show Browser Window (Headed Mode)
              </label>
            </div>
          </div>

          <div style={{ "margin-top": "12px" }}>
            <button
              class="btn-secondary"
              onClick={handleBrowserInit}
              disabled={browserInitRunning()}
            >
              {browserInitRunning() ? "Installing..." : "Initialize Browser"}
            </button>
          </div>

          <Show when={browserInitLog()}>
            <div class="browser-log">{browserInitLog()}</div>
          </Show>
        </section>

        {/* OpenClaw Section */}
        <Show when={formData().mode === "openclaw-gateway"}>
          <section class="config-section">
            <h3>OpenClaw Gateway</h3>
            <div class="form-grid">
              <div class="form-field">
                <label>Gateway Host</label>
                <input
                  type="text"
                  value={formData().openclaw?.gateway_host || ""}
                  onInput={(e) =>
                    updateNestedField("openclaw", "gateway_host", e.currentTarget.value)
                  }
                  placeholder="127.0.0.1"
                />
              </div>

              <div class="form-field">
                <label>Gateway Port</label>
                <input
                  type="number"
                  value={formData().openclaw?.gateway_port || 18789}
                  onInput={(e) =>
                    updateNestedField(
                      "openclaw",
                      "gateway_port",
                      parseInt(e.currentTarget.value)
                    )
                  }
                />
              </div>

              <div class="form-field">
                <label>Display Name</label>
                <input
                  type="text"
                  value={formData().openclaw?.display_name || ""}
                  onInput={(e) =>
                    updateNestedField("openclaw", "display_name", e.currentTarget.value)
                  }
                />
              </div>

              <div class="form-field">
                <label>Auth Token</label>
                <input
                  type="password"
                  value={formData().openclaw?.auth_token || ""}
                  onInput={(e) =>
                    updateNestedField("openclaw", "auth_token", e.currentTarget.value)
                  }
                />
              </div>

              <div class="form-field form-field-checkbox">
                <label>
                  <input
                    type="checkbox"
                    checked={formData().openclaw?.gateway_tls || false}
                    onChange={(e) =>
                      updateNestedField("openclaw", "gateway_tls", e.currentTarget.checked)
                    }
                  />
                  Use TLS
                </label>
              </div>
            </div>
          </section>
        </Show>
      </div>
    );
  }

  // ── Main Render ─────────────────────────────────────────────────

  return (
    <div class="panel">
      <div class="panel-header">
        <h2>Configuration</h2>
        <div class="button-group">
          <Show when={editMode() === "form"}>
            <button onClick={handleEditJson} class="btn-secondary">
              Advanced (JSON)
            </button>
            <button onClick={handleSaveForm} disabled={saving()} class="btn-primary">
              {saving() ? "Saving..." : "Save"}
            </button>
          </Show>
          <Show when={editMode() === "json"}>
            <button onClick={handleEditForm} class="btn-secondary">
              Form Mode
            </button>
            <button onClick={handleSaveJson} disabled={saving()} class="btn-primary">
              {saving() ? "Saving..." : "Save JSON"}
            </button>
          </Show>
        </div>
      </div>

      <Show when={config.loading}>
        <p>Loading...</p>
      </Show>

      <Show when={config.error}>
        <p class="error">Error: {config.error.message}</p>
      </Show>

      <Show when={saveSuccess()}>
        <div class="success-message">Configuration saved successfully!</div>
      </Show>

      <Show when={saveError()}>
        <div class="error-message">{saveError()}</div>
      </Show>

      <Show when={config()}>
        <Show
          when={editMode() === "form"}
          fallback={
            <div class="config-editor">
              <textarea
                class="config-textarea"
                value={jsonValue()}
                onInput={(e) => setJsonValue(e.currentTarget.value)}
                rows={25}
              />
            </div>
          }
        >
          <Show when={viewMode() === "simple"} fallback={<AdvancedForm />}>
            <SimpleForm />
          </Show>
        </Show>
      </Show>
    </div>
  );
}
