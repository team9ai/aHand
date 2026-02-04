import { EventEmitter } from "node:events";
import type {
  Envelope as EnvelopeMsg,
  JobFinished as JobFinishedMsg,
  JobRejected as JobRejectedMsg,
  ApprovalRequest as ApprovalRequestMsg,
} from "@ahand/proto";

export interface JobResult {
  exitCode: number;
  error: string;
}

export class Job extends EventEmitter {
  readonly jobId: string;
  private _resolve!: (result: JobResult) => void;
  private _reject!: (reason: unknown) => void;
  /** Callback set by DeviceConnection to send a CancelJob message. */
  _cancelFn?: () => void;

  /** Resolves when the job completes or is rejected. */
  readonly done: Promise<JobResult>;

  constructor(jobId: string) {
    super();
    this.jobId = jobId;
    this.done = new Promise<JobResult>((resolve, reject) => {
      this._resolve = resolve;
      this._reject = reject;
    });
  }

  /** Request cancellation of this job. */
  cancel(): void {
    this._cancelFn?.();
  }

  /** @internal Called by DeviceConnection when a JobEvent arrives. */
  _onEvent(envelope: EnvelopeMsg): void {
    const ev = envelope.jobEvent;
    if (!ev) return;

    if (ev.stdoutChunk !== undefined) {
      this.emit("stdout", ev.stdoutChunk);
    }
    if (ev.stderrChunk !== undefined) {
      this.emit("stderr", ev.stderrChunk);
    }
    if (ev.progress !== undefined) {
      this.emit("progress", ev.progress);
    }
  }

  /** @internal Called when the job finishes. */
  _onFinished(msg: JobFinishedMsg): void {
    const result: JobResult = {
      exitCode: msg.exitCode,
      error: msg.error,
    };
    this.emit("finished", result);
    this._resolve(result);
  }

  /** @internal Called when the job is rejected by policy. */
  _onRejected(msg: JobRejectedMsg): void {
    this.emit("rejected", msg.reason);
    this._resolve({ exitCode: -1, error: msg.reason });
  }

  /** @internal Called when the job needs approval. */
  _onApprovalRequest(msg: ApprovalRequestMsg): void {
    this.emit("approvalRequest", msg);
  }
}
