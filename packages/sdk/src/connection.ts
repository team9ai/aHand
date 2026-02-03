import type WebSocket from "ws";
import type { Hello as HelloMsg, Envelope as EnvelopeMsg } from "@ahand/proto";
import { encodeEnvelope, makeEnvelope, decodeEnvelope } from "./codec.ts";
import { Job } from "./job.ts";

export interface ExecOptions {
  cwd?: string;
  env?: Record<string, string>;
  timeoutMs?: number;
}

export class DeviceConnection {
  readonly deviceId: string;
  readonly hello: HelloMsg;
  private readonly _ws: WebSocket;
  private readonly _jobs = new Map<string, Job>();

  constructor(deviceId: string, hello: HelloMsg, ws: WebSocket) {
    this.deviceId = deviceId;
    this.hello = hello;
    this._ws = ws;

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

    this._ws.send(encodeEnvelope(envelope));

    job._cancelFn = () => this.cancelJob(jobId);

    return job;
  }

  cancelJob(jobId: string): void {
    const envelope = makeEnvelope(this.deviceId, {
      cancelJob: { jobId },
    });
    this._ws.send(encodeEnvelope(envelope));
  }

  close(): void {
    this._ws.close();
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

  private _handleMessage(raw: Buffer): void {
    let envelope: EnvelopeMsg;
    try {
      envelope = decodeEnvelope(new Uint8Array(raw));
    } catch {
      return;
    }

    if (envelope.jobEvent) {
      const job = this._jobs.get(envelope.jobEvent.jobId);
      job?._onEvent(envelope);
    } else if (envelope.jobFinished) {
      const job = this._jobs.get(envelope.jobFinished.jobId);
      if (job) {
        job._onFinished(envelope.jobFinished);
        this._jobs.delete(envelope.jobFinished.jobId);
      }
    } else if (envelope.jobRejected) {
      const job = this._jobs.get(envelope.jobRejected.jobId);
      if (job) {
        job._onRejected(envelope.jobRejected);
        this._jobs.delete(envelope.jobRejected.jobId);
      }
    }
  }
}
