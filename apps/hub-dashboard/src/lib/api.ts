import { cookies } from "next/headers";

export type DashboardStats = {
  online_devices: number;
  offline_devices: number;
  running_jobs: number;
};

export type DeviceRecord = {
  id: string;
  public_key: number[] | null;
  hostname: string;
  os: string;
  capabilities: string[];
  version: string | null;
  auth_method: string;
  online: boolean;
};

export type JobRecord = {
  id: string;
  device_id: string;
  tool: string;
  args: string[];
  cwd: string | null;
  env: Record<string, string>;
  timeout_ms: number;
  status: string;
  requested_by: string;
};

export type AuditLogRecord = {
  timestamp: string;
  action: string;
  resource_type: string;
  resource_id: string;
  actor: string;
  detail: Record<string, unknown> | unknown[];
};

type JobFilters = {
  deviceId?: string;
  status?: string;
};

type AuditFilters = {
  action?: string;
  resource?: string;
  since?: string;
  until?: string;
  limit?: number;
};

export async function apiGet<T>(path: string): Promise<T> {
  const token = await readSessionToken();
  const baseUrl = process.env.AHAND_HUB_BASE_URL;

  if (!baseUrl) {
    throw new Error("hub_unavailable");
  }

  if (!token) {
    throw new Error("unauthorized");
  }

  const response = await fetch(buildHubUrl(baseUrl, path), {
    headers: {
      accept: "application/json",
      authorization: `Bearer ${token}`,
    },
    cache: "no-store",
  });

  if (!response.ok) {
    throw new Error(`api_${response.status}`);
  }

  return (await response.json()) as T;
}

export async function getDashboardStats() {
  return apiGet<DashboardStats>("/api/stats");
}

export async function getDevices() {
  return apiGet<DeviceRecord[]>("/api/devices");
}

export async function getDevice(id: string) {
  try {
    return await apiGet<DeviceRecord>(`/api/devices/${id}`);
  } catch (error) {
    if (error instanceof Error && error.message === "api_404") {
      return null;
    }
    throw error;
  }
}

export async function getJobs(filters: JobFilters = {}) {
  const search = new URLSearchParams();
  if (filters.deviceId) {
    search.set("device_id", filters.deviceId);
  }
  if (filters.status) {
    search.set("status", filters.status);
  }
  const suffix = search.toString();
  return apiGet<JobRecord[]>(`/api/jobs${suffix ? `?${suffix}` : ""}`);
}

export async function getJob(id: string) {
  try {
    return await apiGet<JobRecord>(`/api/jobs/${id}`);
  } catch (error) {
    if (error instanceof Error && error.message === "api_404") {
      return null;
    }
    throw error;
  }
}

export async function getAuditLogs(filters: AuditFilters = {}) {
  const search = new URLSearchParams();
  if (filters.action) {
    search.set("action", filters.action);
  }
  if (filters.resource) {
    search.set("resource", filters.resource);
  }
  if (filters.since) {
    search.set("since", filters.since);
  }
  if (filters.until) {
    search.set("until", filters.until);
  }
  if (typeof filters.limit === "number") {
    search.set("limit", String(filters.limit));
  }

  const suffix = search.toString();
  return apiGet<AuditLogRecord[]>(`/api/audit-logs${suffix ? `?${suffix}` : ""}`);
}

async function readSessionToken() {
  const cookieStore = await cookies();
  return cookieStore.get("ahand_hub_session")?.value ?? null;
}

function buildHubUrl(baseUrl: string, path: string) {
  return new URL(path, `${baseUrl.replace(/\/?$/, "/")}`).toString();
}
