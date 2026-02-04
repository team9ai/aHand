import { EventEmitter } from "node:events";
import type WebSocket from "ws";
import type {
  Hello as HelloMsg,
  Envelope as EnvelopeMsg,
  PolicyUpdate as PolicyUpdateMsg,
} from "@ahand/proto";
import { makeEnvelope, decodeEnvelope } from "./codec.ts";
import { Job } from "./job.ts";
import { Outbox, prepareOutbound } from "./outbox.ts";

export interface ExecOptions {
  cwd?: string;
  env?: Record<string, string>;
  timeoutMs?: number;
}

export class DeviceConnection extends EventEmitter {
  readonly deviceId: string;
  readonly hello: HelloMsg;
  private readonly _ws: WebSocket;
  private readonly _jobs = new Map<string, Job>();
  /** Outbox for seq/ack tracking. Injected by AHandServer on reconnect. */
  private _outbox: Outbox;

  constructor(
    deviceId: string,
    hello: HelloMsg,
    ws: WebSocket,
    outbox?: Outbox,
  ) {
    super();
    this.deviceId = deviceId;
    this.hello = hello;
    this._ws = ws;
    this._outbox = outbox ?? new Outbox();

    this._ws.on("message", (raw: Buffer) => {
      this._handleMessage(raw);
    });
  }

  get hostname(): string {
    return this.hello.hostname;
  }

  get os(): string {
    return this.hello.os;
  }

  get capabilities(): string[] {
    return this.hello.capabilities;
  }

  get connected(): boolean {
    return this._ws.readyState === this._ws.OPEN;
  }

  /** @internal Access the outbox (used by AHandServer for reconnect transfer). */
  get outbox(): Outbox {
    return this._outbox;
  }

  exec(tool: string, args: string[], opts?: ExecOptions): Job {
    const jobId = crypto.randomUUID();
    const job = new Job(jobId);
    this._jobs.set(jobId, job);

    const envelope = makeEnvelope(this.deviceId, {
      jobRequest: {
        jobId,
        tool,
        args,
        cwd: opts?.cwd ?? "",
        env: opts?.env ?? {},
        timeoutMs: opts?.timeoutMs ?? 0,
      },
    });

    this._send(envelope);

    job._cancelFn = () => this.cancelJob(jobId);

    return job;
  }

  cancelJob(jobId: string): void {
    const envelope = makeEnvelope(this.deviceId, {
      cancelJob: { jobId },
    });
    this._send(envelope);
  }

  close(): void {
    this._ws.close();
  }

  /** Send an approval response for a pending job. */
  approveJob(jobId: string, approved: boolean, remember = false, reason = ""): void {
    const envelope = makeEnvelope(this.deviceId, {
      approvalResponse: { jobId, approved, remember, reason },
    });
    this._send(envelope);
  }

  /** Request the current policy state. Listen for the "policyState" event. */
  queryPolicy(): void {
    const envelope = makeEnvelope(this.deviceId, {
      policyQuery: {},
    });
    this._send(envelope);
  }

  /** Send a policy update. The daemon responds with a "policyState" event. */
  updatePolicy(update: PolicyUpdateMsg): void {
    const envelope = makeEnvelope(this.deviceId, {
      policyUpdate: update,
    });
    this._send(envelope);
  }

  /** Set session mode for a caller. The daemon responds with a "sessionState" event. */
  setSessionMode(callerUid: string, mode: number, trustTimeoutMins = 0): void {
    const envelope = makeEnvelope(this.deviceId, {
      setSessionMode: { callerUid, mode, trustTimeoutMins },
    });
    this._send(envelope);
  }

  /** Query session state. Empty callerUid = query all sessions. */
  querySession(callerUid = ""): void {
    const envelope = makeEnvelope(this.deviceId, {
      sessionQuery: { callerUid },
    });
    this._send(envelope);
  }

  toJSON(): object {
    return {
      deviceId: this.deviceId,
      hostname: this.hello.hostname,
      os: this.hello.os,
      capabilities: this.hello.capabilities,
      connected: this.connected,
    };
  }

  /** Stamp via outbox, encode, and send over WS. */
  private _send(envelope: EnvelopeMsg): void {
    const data = prepareOutbound(this._outbox, envelope);
    this._ws.send(data);
  }

  private _handleMessage(raw: Buffer): void {
    let envelope: EnvelopeMsg;
    try {
      envelope = decodeEnvelope(new Uint8Array(raw));
    } catch {
      return;
    }

    // Update outbox with peer's seq and ack.
    if (envelope.seq > 0) {
      this._outbox.onRecv(envelope.seq);
    }
    if (envelope.ack > 0) {
      this._outbox.onPeerAck(envelope.ack);
    }

    if (envelope.jobEvent) {
      const job = this._jobs.get(envelope.jobEvent.jobId);
      job?._onEvent(envelope);
    } else if (envelope.jobFinished) {
      const jf = envelope.jobFinished;
      const job = this._jobs.get(jf.jobId);
      console.log(`[sdk] jobFinished jobId=${jf.jobId} found=${!!job} exit=${jf.exitCode} err=${jf.error}`);
      if (job) {
        job._onFinished(jf);
        this._jobs.delete(jf.jobId);
      }
    } else if (envelope.jobRejected) {
      const jr = envelope.jobRejected;
      const job = this._jobs.get(jr.jobId);
      console.log(`[sdk] jobRejected jobId=${jr.jobId} found=${!!job} reason=${jr.reason}`);
      if (job) {
        job._onRejected(jr);
        this._jobs.delete(jr.jobId);
      }
    } else if (envelope.approvalRequest) {
      const job = this._jobs.get(envelope.approvalRequest.jobId);
      job?._onApprovalRequest(envelope.approvalRequest);
      this.emit("approvalRequest", envelope.approvalRequest);
    } else if (envelope.policyState) {
      this.emit("policyState", envelope.policyState);
    } else if (envelope.sessionState) {
      this.emit("sessionState", envelope.sessionState);
    }
  }
}
