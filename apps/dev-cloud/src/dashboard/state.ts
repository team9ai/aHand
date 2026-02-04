import type { Job } from "@ahand/sdk";
import type {
  ActiveJob,
  ApprovalRequestEvent,
  DeviceInfo,
  AnyDashboardEvent,
  SnapshotEvent,
} from "./types.ts";

const MAX_OUTPUT_BYTES = 64 * 1024; // 64 KB per job
const MAX_EVENT_LOG = 500;

interface TrackedJob {
  jobId: string;
  deviceId: string;
  tool: string;
  args: string[];
  cwd: string;
  status: "running" | "pending_approval";
  stdout: string;
  stderr: string;
  job: Job;
}

export class DashboardState {
  private _jobs = new Map<string, TrackedJob>();
  private _approvals = new Map<string, ApprovalRequestEvent>();
  private _eventLog: AnyDashboardEvent[] = [];

  // ── Job tracking ───────────────────────────────────────────────

  trackJob(
    jobId: string,
    deviceId: string,
    tool: string,
    args: string[],
    cwd: string,
    job: Job,
  ): void {
    this._jobs.set(jobId, {
      jobId,
      deviceId,
      tool,
      args,
      cwd,
      status: "running",
      stdout: "",
      stderr: "",
      job,
    });
  }

  appendStdout(jobId: string, data: string): void {
    const j = this._jobs.get(jobId);
    if (!j) return;
    if (j.stdout.length < MAX_OUTPUT_BYTES) {
      j.stdout += data;
    }
  }

  appendStderr(jobId: string, data: string): void {
    const j = this._jobs.get(jobId);
    if (!j) return;
    if (j.stderr.length < MAX_OUTPUT_BYTES) {
      j.stderr += data;
    }
  }

  finishJob(jobId: string): void {
    this._jobs.delete(jobId);
  }

  rejectJob(jobId: string): void {
    this._jobs.delete(jobId);
  }

  setJobPendingApproval(jobId: string): void {
    const j = this._jobs.get(jobId);
    if (j) j.status = "pending_approval";
  }

  getActiveJobs(): ActiveJob[] {
    return Array.from(this._jobs.values()).map((j) => ({
      jobId: j.jobId,
      deviceId: j.deviceId,
      tool: j.tool,
      args: j.args,
      cwd: j.cwd,
      status: j.status,
      stdout: j.stdout,
      stderr: j.stderr,
    }));
  }

  // ── Approval tracking ──────────────────────────────────────────

  addApproval(evt: ApprovalRequestEvent): void {
    this._approvals.set(evt.jobId, evt);
  }

  resolveApproval(jobId: string): void {
    this._approvals.delete(jobId);
  }

  getPendingApprovals(): ApprovalRequestEvent[] {
    return Array.from(this._approvals.values());
  }

  // ── Event log ──────────────────────────────────────────────────

  pushEvent(evt: AnyDashboardEvent): void {
    this._eventLog.push(evt);
    if (this._eventLog.length > MAX_EVENT_LOG) {
      this._eventLog.shift();
    }
  }

  getEventLog(): AnyDashboardEvent[] {
    return this._eventLog;
  }

  // ── Snapshot ───────────────────────────────────────────────────

  getSnapshot(devices: DeviceInfo[]): SnapshotEvent {
    return {
      type: "snapshot",
      ts: Date.now(),
      devices,
      activeJobs: this.getActiveJobs(),
      pendingApprovals: this.getPendingApprovals(),
    };
  }
}
