// ── Dashboard WebSocket Event Types ──────────────────────────────
// All events are pushed server → client as JSON via /dashboard/ws.

export interface DashboardEvent {
  type: string;
  ts: number;
}

// ── Device Events ────────────────────────────────────────────────

export interface DeviceInfo {
  deviceId: string;
  hostname: string;
  os: string;
  capabilities: string[];
  connected: boolean;
}

export interface DeviceConnectedEvent extends DashboardEvent {
  type: "device.connected";
  device: DeviceInfo;
}

export interface DeviceDisconnectedEvent extends DashboardEvent {
  type: "device.disconnected";
  deviceId: string;
}

// ── Job Events ───────────────────────────────────────────────────

export interface JobStartedEvent extends DashboardEvent {
  type: "job.started";
  jobId: string;
  deviceId: string;
  tool: string;
  args: string[];
  cwd: string;
}

export interface JobStdoutEvent extends DashboardEvent {
  type: "job.stdout";
  jobId: string;
  data: string;
}

export interface JobStderrEvent extends DashboardEvent {
  type: "job.stderr";
  jobId: string;
  data: string;
}

export interface JobProgressEvent extends DashboardEvent {
  type: "job.progress";
  jobId: string;
  progress: number;
}

export interface JobFinishedEvent extends DashboardEvent {
  type: "job.finished";
  jobId: string;
  exitCode: number;
  error: string;
}

export interface JobRejectedEvent extends DashboardEvent {
  type: "job.rejected";
  jobId: string;
  reason: string;
}

// ── Approval Events ──────────────────────────────────────────────

export interface RefusalContext {
  tool: string;
  reason: string;
  refusedAtMs: number;
}

export interface ApprovalRequestEvent extends DashboardEvent {
  type: "approval.request";
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

export interface ApprovalResolvedEvent extends DashboardEvent {
  type: "approval.resolved";
  jobId: string;
  approved: boolean;
}

// ── Session Events ──────────────────────────────────────────────

export type SessionModeString = "inactive" | "strict" | "trust" | "auto_accept";

export interface SessionInfo {
  callerUid: string;
  mode: SessionModeString;
  trustExpiresMs: number;
  trustTimeoutMins: number;
}

export interface SessionStateEvent extends DashboardEvent {
  type: "session.state";
  deviceId: string;
  session: SessionInfo;
}

// ── Policy Events ────────────────────────────────────────────────

export interface PolicyStatePayload {
  allowedTools: string[];
  deniedTools: string[];
  deniedPaths: string[];
  allowedDomains: string[];
  approvalTimeoutSecs: number;
}

export interface PolicyStateEvent extends DashboardEvent {
  type: "policy.state";
  deviceId: string;
  policy: PolicyStatePayload;
}

// ── Snapshot (sent on WS connect) ────────────────────────────────

export interface ActiveJob {
  jobId: string;
  deviceId: string;
  tool: string;
  args: string[];
  cwd: string;
  status: "running" | "pending_approval";
  stdout: string;
  stderr: string;
}

export interface SnapshotEvent extends DashboardEvent {
  type: "snapshot";
  devices: DeviceInfo[];
  activeJobs: ActiveJob[];
  pendingApprovals: ApprovalRequestEvent[];
}

export type AnyDashboardEvent =
  | SnapshotEvent
  | DeviceConnectedEvent
  | DeviceDisconnectedEvent
  | JobStartedEvent
  | JobStdoutEvent
  | JobStderrEvent
  | JobProgressEvent
  | JobFinishedEvent
  | JobRejectedEvent
  | ApprovalRequestEvent
  | ApprovalResolvedEvent
  | PolicyStateEvent
  | SessionStateEvent;
