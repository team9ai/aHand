import { Envelope } from "@ahand/proto";
import type { Envelope as EnvelopeMsg } from "@ahand/proto";

export function encodeEnvelope(msg: EnvelopeMsg): Uint8Array {
  return Envelope.encode(msg).finish();
}

export function decodeEnvelope(data: Uint8Array): EnvelopeMsg {
  return Envelope.decode(data);
}

export function nextMsgId(): string {
  return crypto.randomUUID();
}

/**
 * Build an Envelope with seq=0 and ack=0.
 * The Outbox will overwrite seq/ack via stamp() before sending.
 */
export function makeEnvelope(
  deviceId: string,
  payload: Partial<EnvelopeMsg>,
): EnvelopeMsg {
  return {
    deviceId,
    traceId: payload.traceId ?? "",
    msgId: nextMsgId(),
    seq: 0,
    ack: 0,
    tsMs: Date.now(),
    hello: payload.hello,
    jobRequest: payload.jobRequest,
    jobEvent: payload.jobEvent,
    jobFinished: payload.jobFinished,
    jobRejected: payload.jobRejected,
    cancelJob: payload.cancelJob,
    approvalRequest: payload.approvalRequest,
    approvalResponse: payload.approvalResponse,
    policyQuery: payload.policyQuery,
    policyState: payload.policyState,
    policyUpdate: payload.policyUpdate,
    setSessionMode: payload.setSessionMode,
    sessionState: payload.sessionState,
    sessionQuery: payload.sessionQuery,
  };
}
