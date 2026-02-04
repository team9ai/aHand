import type WebSocket from "ws";
import type { AHandServer, DeviceConnection, Job } from "@ahand/sdk";
import type { DashboardState } from "./state.ts";
import type {
  AnyDashboardEvent,
  DeviceInfo,
  ApprovalRequestEvent,
  SessionModeString,
} from "./types.ts";

export class DashboardBroadcaster {
  private _clients = new Set<WebSocket>();
  private _state: DashboardState;
  private _ahand: AHandServer;

  constructor(ahand: AHandServer, state: DashboardState) {
    this._ahand = ahand;
    this._state = state;
    this._attach();
  }

  addClient(ws: WebSocket): void {
    this._clients.add(ws);

    // Send initial snapshot.
    const devices = this._ahand.devices().map(deviceToInfo);
    const snapshot = this._state.getSnapshot(devices);
    ws.send(JSON.stringify(snapshot));

    ws.on("close", () => {
      this._clients.delete(ws);
    });
  }

  broadcast(event: AnyDashboardEvent): void {
    this._state.pushEvent(event);
    const data = JSON.stringify(event);
    let sent = 0;
    for (const ws of this._clients) {
      if (ws.readyState === ws.OPEN) {
        ws.send(data);
        sent++;
      }
    }
    if (event.type === "job.finished" || event.type === "job.rejected") {
      console.log(`[broadcaster] broadcast ${event.type} to ${sent}/${this._clients.size} clients`);
    }
  }

  /** Wire up a Job's events for broadcasting. */
  wireJob(
    jobId: string,
    deviceId: string,
    tool: string,
    args: string[],
    cwd: string,
    job: Job,
  ): void {
    this._state.trackJob(jobId, deviceId, tool, args, cwd, job);

    this.broadcast({
      type: "job.started",
      ts: Date.now(),
      jobId,
      deviceId,
      tool,
      args,
      cwd,
    });

    job.on("stdout", (chunk: Uint8Array) => {
      const text = new TextDecoder().decode(chunk);
      this._state.appendStdout(jobId, text);
      this.broadcast({ type: "job.stdout", ts: Date.now(), jobId, data: text });
    });

    job.on("stderr", (chunk: Uint8Array) => {
      const text = new TextDecoder().decode(chunk);
      this._state.appendStderr(jobId, text);
      this.broadcast({ type: "job.stderr", ts: Date.now(), jobId, data: text });
    });

    job.on("progress", (progress: number) => {
      this.broadcast({ type: "job.progress", ts: Date.now(), jobId, progress });
    });

    job.on("finished", (result: { exitCode: number; error: string }) => {
      console.log(`[broadcaster] job.finished: ${jobId} exitCode=${result.exitCode} error=${result.error}`);
      this._state.finishJob(jobId);
      this.broadcast({
        type: "job.finished",
        ts: Date.now(),
        jobId,
        exitCode: result.exitCode,
        error: result.error,
      });
    });

    job.on("rejected", (reason: string) => {
      console.log(`[broadcaster] job.rejected: ${jobId} reason=${reason}`);
      this._state.rejectJob(jobId);
      this.broadcast({ type: "job.rejected", ts: Date.now(), jobId, reason });
    });

    // NOTE: approvalRequest is handled in _wireDevice to avoid duplicate broadcasts.
  }

  private _attach(): void {
    this._ahand.onDevice((conn) => {
      this.broadcast({
        type: "device.connected",
        ts: Date.now(),
        device: deviceToInfo(conn),
      });
      this._wireDevice(conn);
    });

    this._ahand.on("deviceDisconnected", (conn: DeviceConnection) => {
      this.broadcast({
        type: "device.disconnected",
        ts: Date.now(),
        deviceId: conn.deviceId,
      });
    });
  }

  private _wireDevice(conn: DeviceConnection): void {
    conn.on(
      "approvalRequest",
      (req: {
        jobId: string;
        tool: string;
        args: string[];
        cwd: string;
        reason: string;
        detectedDomains: string[];
        expiresMs: number;
        callerUid: string;
        previousRefusals: { tool: string; reason: string; refusedAtMs: number }[];
      }) => {
        this._state.setJobPendingApproval(req.jobId);
        const evt: ApprovalRequestEvent = {
          type: "approval.request",
          ts: Date.now(),
          jobId: req.jobId,
          tool: req.tool,
          args: req.args,
          cwd: req.cwd,
          reason: req.reason,
          detectedDomains: req.detectedDomains,
          expiresMs: Number(req.expiresMs),
          callerUid: req.callerUid,
          previousRefusals: (req.previousRefusals ?? []).map((r) => ({
            tool: r.tool,
            reason: r.reason,
            refusedAtMs: Number(r.refusedAtMs),
          })),
        };
        this._state.addApproval(evt);
        this.broadcast(evt);
      },
    );

    conn.on(
      "policyState",
      (state: {
        allowedTools: string[];
        deniedTools: string[];
        deniedPaths: string[];
        allowedDomains: string[];
        approvalTimeoutSecs: number;
      }) => {
        this.broadcast({
          type: "policy.state",
          ts: Date.now(),
          deviceId: conn.deviceId,
          policy: {
            allowedTools: state.allowedTools,
            deniedTools: state.deniedTools,
            deniedPaths: state.deniedPaths,
            allowedDomains: state.allowedDomains,
            approvalTimeoutSecs: Number(state.approvalTimeoutSecs),
          },
        });
      },
    );

    conn.on(
      "sessionState",
      (state: {
        callerUid: string;
        mode: number;
        trustExpiresMs: number;
        trustTimeoutMins: number;
      }) => {
        const modeNames: Record<number, SessionModeString> = {
          0: "inactive",
          1: "strict",
          2: "trust",
          3: "auto_accept",
        };
        this.broadcast({
          type: "session.state",
          ts: Date.now(),
          deviceId: conn.deviceId,
          session: {
            callerUid: state.callerUid,
            mode: modeNames[state.mode] ?? "inactive",
            trustExpiresMs: Number(state.trustExpiresMs),
            trustTimeoutMins: Number(state.trustTimeoutMins),
          },
        });
      },
    );
  }
}

function deviceToInfo(conn: DeviceConnection): DeviceInfo {
  return {
    deviceId: conn.deviceId,
    hostname: conn.hostname,
    os: conn.os,
    capabilities: conn.capabilities,
    connected: conn.connected,
  };
}
