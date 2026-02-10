// ──────────────────────────────────────────────────────────────────────
// Token management
// ──────────────────────────────────────────────────────────────────────

function getToken(): string | null {
  // First check URL query parameter (higher priority)
  const params = new URLSearchParams(window.location.search);
  const urlToken = params.get("token");
  if (urlToken) {
    localStorage.setItem("ahand_admin_token", urlToken);
    // Clean URL
    window.history.replaceState({}, document.title, window.location.pathname);
    return urlToken;
  }

  // Then check localStorage
  const stored = localStorage.getItem("ahand_admin_token");
  if (stored) {
    return stored;
  }

  return null;
}

// ──────────────────────────────────────────────────────────────────────
// API client
// ──────────────────────────────────────────────────────────────────────

async function fetchAPI<T>(path: string, options?: RequestInit): Promise<T> {
  const token = getToken();
  if (!token) {
    throw new Error("No authentication token found");
  }

  const headers = {
    "Content-Type": "application/json",
    Authorization: `Bearer ${token}`,
    ...options?.headers,
  };

  const response = await fetch(`/api${path}`, {
    ...options,
    headers,
  });

  if (!response.ok) {
    throw new Error(`API error: ${response.statusText}`);
  }

  return response.json();
}

async function fetchText(path: string): Promise<string> {
  const token = getToken();
  if (!token) {
    throw new Error("No authentication token found");
  }

  const response = await fetch(`/api${path}`, {
    headers: {
      Authorization: `Bearer ${token}`,
    },
  });

  if (!response.ok) {
    throw new Error(`API error: ${response.statusText}`);
  }

  return response.text();
}

// ──────────────────────────────────────────────────────────────────────
// API endpoints
// ──────────────────────────────────────────────────────────────────────

export interface StatusResponse {
  version: string;
  daemon_running: boolean;
  daemon_pid: number | null;
  config_path: string;
  data_dir: string;
  data_dir_size: number;
}

export interface LogEntry {
  ts_ms: number;
  direction: string;
  device_id: string;
  msg_id: string;
  seq: number;
  ack: number;
  payload_type: string;
}

export interface LogsResponse {
  total: number;
  entries: LogEntry[];
}

export interface RunEntry {
  job_id: string;
  created_at: number;
}

export interface RunsResponse {
  total: number;
  runs: RunEntry[];
}

export interface RunDetail {
  job_id: string;
  request: any;
  result: any | null;
  files: string[];
}

export const api = {
  async getStatus(): Promise<StatusResponse> {
    return fetchAPI("/status");
  },

  async getConfig(): Promise<any> {
    return fetchAPI("/config");
  },

  async putConfig(config: any): Promise<void> {
    await fetch("/api/config", {
      method: "PUT",
      headers: {
        "Content-Type": "application/json",
        Authorization: `Bearer ${getToken()}`,
      },
      body: JSON.stringify(config),
    });
  },

  async getLogs(limit: number = 50, offset: number = 0): Promise<LogsResponse> {
    return fetchAPI(`/logs?limit=${limit}&offset=${offset}`);
  },

  async getRuns(limit: number = 20, offset: number = 0): Promise<RunsResponse> {
    return fetchAPI(`/runs?limit=${limit}&offset=${offset}`);
  },

  async getRunDetail(jobId: string): Promise<RunDetail> {
    return fetchAPI(`/runs/${jobId}`);
  },

  async getRunFile(jobId: string, filename: string): Promise<string> {
    return fetchText(`/runs/${jobId}/${filename}`);
  },
};

export { getToken };
