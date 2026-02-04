import { createStore, produce } from "solid-js/store";

// ── Types ────────────────────────────────────────────────────────

export interface DeviceInfo {
  deviceId: string;
  hostname: string;
  os: string;
  capabilities: string[];
  connected: boolean;
}

export interface JobState {
  jobId: string;
  deviceId: string;
  tool: string;
  args: string[];
  cwd: string;
  status: "running" | "pending_approval" | "finished" | "rejected";
  stdout: string;
  stderr: string;
  exitCode?: number;
  error?: string;
  progress?: number;
}

export interface RefusalContext {
  tool: string;
  reason: string;
  refusedAtMs: number;
}

export interface ApprovalInfo {
  jobId: string;
  tool: string;
  args: string[];
  cwd: string;
  reason: string;
  detectedDomains: string[];
  expiresMs: number;
  callerUid: string;
  previousRefusals: RefusalContext[];
}

export type SessionModeString = "inactive" | "strict" | "trust" | "auto_accept";

export interface SessionInfo {
  callerUid: string;
  mode: SessionModeString;
  trustExpiresMs: number;
  trustTimeoutMins: number;
}

export interface PolicyState {
  allowedTools: string[];
  deniedTools: string[];
  deniedPaths: string[];
  allowedDomains: string[];
  approvalTimeoutSecs: number;
}

export interface DashEvent {
  type: string;
  ts: number;
  [key: string]: unknown;
}

export interface DashboardStore {
  devices: DeviceInfo[];
  jobs: JobState[];
  pendingApprovals: ApprovalInfo[];
  policyByDevice: Record<string, PolicyState>;
  sessionByDevice: Record<string, Record<string, SessionInfo>>;
  events: DashEvent[];
  wsConnected: boolean;
}

// ── Store ────────────────────────────────────────────────────────

const MAX_EVENTS = 200;
const MAX_OUTPUT = 256 * 1024;

export const [store, setStore] = createStore<DashboardStore>({
  devices: [],
  jobs: [],
  pendingApprovals: [],
  policyByDevice: {},
  sessionByDevice: {},
  events: [],
  wsConnected: false,
});

// ── Event Handler ────────────────────────────────────────────────

export function handleEvent(evt: Record<string, unknown>): void {
  const type = evt.type as string;

  if (type === "job.finished" || type === "job.rejected") {
    console.log(`[dashboard] received ${type}`, evt);
  }

  // Push to event log.
  setStore(
    produce((s) => {
      s.events.push(evt as DashEvent);
      if (s.events.length > MAX_EVENTS) {
        s.events.splice(0, s.events.length - MAX_EVENTS);
      }
    }),
  );

  switch (type) {
    case "snapshot":
      handleSnapshot(evt);
      break;
    case "device.connected":
      handleDeviceConnected(evt);
      break;
    case "device.disconnected":
      handleDeviceDisconnected(evt);
      break;
    case "job.started":
      handleJobStarted(evt);
      break;
    case "job.stdout":
      handleJobOutput(evt, "stdout");
      break;
    case "job.stderr":
      handleJobOutput(evt, "stderr");
      break;
    case "job.progress":
      handleJobProgress(evt);
      break;
    case "job.finished":
      handleJobFinished(evt);
      break;
    case "job.rejected":
      handleJobRejected(evt);
      break;
    case "approval.request":
      handleApprovalRequest(evt);
      break;
    case "approval.resolved":
      handleApprovalResolved(evt);
      break;
    case "policy.state":
      handlePolicyState(evt);
      break;
    case "session.state":
      handleSessionState(evt);
      break;
  }
}

function handleSnapshot(evt: Record<string, unknown>): void {
  const devices = evt.devices as DeviceInfo[];
  const activeJobs = evt.activeJobs as Array<Record<string, unknown>>;
  const approvals = evt.pendingApprovals as ApprovalInfo[];

  setStore(
    produce((s) => {
      s.devices = devices;
      s.jobs = activeJobs.map((j) => ({
        jobId: j.jobId as string,
        deviceId: j.deviceId as string,
        tool: j.tool as string,
        args: j.args as string[],
        cwd: j.cwd as string,
        status: j.status as "running" | "pending_approval",
        stdout: j.stdout as string,
        stderr: j.stderr as string,
      }));
      s.pendingApprovals = approvals;
    }),
  );
}

function handleDeviceConnected(evt: Record<string, unknown>): void {
  const device = evt.device as DeviceInfo;
  setStore(
    produce((s) => {
      const idx = s.devices.findIndex((d) => d.deviceId === device.deviceId);
      if (idx >= 0) {
        s.devices[idx] = device;
      } else {
        s.devices.push(device);
      }
    }),
  );
}

function handleDeviceDisconnected(evt: Record<string, unknown>): void {
  const deviceId = evt.deviceId as string;
  setStore(
    produce((s) => {
      const idx = s.devices.findIndex((d) => d.deviceId === deviceId);
      if (idx >= 0) {
        s.devices[idx].connected = false;
      }
    }),
  );
}

function handleJobStarted(evt: Record<string, unknown>): void {
  setStore(
    produce((s) => {
      s.jobs.push({
        jobId: evt.jobId as string,
        deviceId: evt.deviceId as string,
        tool: evt.tool as string,
        args: evt.args as string[],
        cwd: evt.cwd as string,
        status: "running",
        stdout: "",
        stderr: "",
      });
    }),
  );
}

function handleJobOutput(
  evt: Record<string, unknown>,
  stream: "stdout" | "stderr",
): void {
  const jobId = evt.jobId as string;
  const data = evt.data as string;
  setStore(
    produce((s) => {
      const job = s.jobs.find((j) => j.jobId === jobId);
      if (job && job[stream].length < MAX_OUTPUT) {
        job[stream] += data;
      }
    }),
  );
}

function handleJobProgress(evt: Record<string, unknown>): void {
  const jobId = evt.jobId as string;
  setStore(
    produce((s) => {
      const job = s.jobs.find((j) => j.jobId === jobId);
      if (job) job.progress = evt.progress as number;
    }),
  );
}

function handleJobFinished(evt: Record<string, unknown>): void {
  const jobId = evt.jobId as string;
  setStore(
    produce((s) => {
      const job = s.jobs.find((j) => j.jobId === jobId);
      if (job) {
        job.status = "finished";
        job.exitCode = evt.exitCode as number;
        job.error = evt.error as string;
      }
    }),
  );
}

function handleJobRejected(evt: Record<string, unknown>): void {
  const jobId = evt.jobId as string;
  setStore(
    produce((s) => {
      const job = s.jobs.find((j) => j.jobId === jobId);
      if (job) {
        job.status = "rejected";
        job.error = evt.reason as string;
      }
    }),
  );
}

function handleApprovalRequest(evt: Record<string, unknown>): void {
  setStore(
    produce((s) => {
      s.pendingApprovals.push({
        jobId: evt.jobId as string,
        tool: evt.tool as string,
        args: evt.args as string[],
        cwd: evt.cwd as string,
        reason: evt.reason as string,
        detectedDomains: evt.detectedDomains as string[],
        expiresMs: evt.expiresMs as number,
        callerUid: evt.callerUid as string,
        previousRefusals: (evt.previousRefusals as RefusalContext[]) ?? [],
      });
    }),
  );
}

function handleApprovalResolved(evt: Record<string, unknown>): void {
  const jobId = evt.jobId as string;
  setStore(
    produce((s) => {
      s.pendingApprovals = s.pendingApprovals.filter(
        (a) => a.jobId !== jobId,
      );
    }),
  );
}

function handlePolicyState(evt: Record<string, unknown>): void {
  const deviceId = evt.deviceId as string;
  const policy = evt.policy as PolicyState;
  setStore("policyByDevice", deviceId, policy);
}

function handleSessionState(evt: Record<string, unknown>): void {
  const deviceId = evt.deviceId as string;
  const session = evt.session as SessionInfo;
  setStore(
    produce((s) => {
      if (!s.sessionByDevice[deviceId]) {
        s.sessionByDevice[deviceId] = {};
      }
      s.sessionByDevice[deviceId][session.callerUid] = session;
    }),
  );
}
