import { Envelope } from "@ahand/proto";
import type { Envelope as EnvelopeMsg } from "@ahand/proto";

export function encodeEnvelope(msg: EnvelopeMsg): Uint8Array {
  return Envelope.encode(msg).finish();
}

export function decodeEnvelope(data: Uint8Array): EnvelopeMsg {
  return Envelope.decode(data);
}

let _seq = 0;

export function nextMsgId(): string {
  return crypto.randomUUID();
}

export function nextSeq(): number {
  return ++_seq;
}

export function makeEnvelope(
  deviceId: string,
  payload: Partial<EnvelopeMsg>,
): EnvelopeMsg {
  return {
    deviceId,
    traceId: payload.traceId ?? "",
    msgId: nextMsgId(),
    seq: nextSeq(),
    ack: payload.ack ?? 0,
    tsMs: Date.now(),
    hello: payload.hello,
    jobRequest: payload.jobRequest,
    jobEvent: payload.jobEvent,
    jobFinished: payload.jobFinished,
    jobRejected: payload.jobRejected,
    cancelJob: payload.cancelJob,
  };
}
