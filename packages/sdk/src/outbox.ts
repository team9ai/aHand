import type { Envelope as EnvelopeMsg } from "@ahand/proto";
import { encodeEnvelope } from "./codec.ts";

/**
 * Tracks outbound seq/ack and buffers unacknowledged messages for replay
 * on reconnect.
 */
export class Outbox {
  private _nextSeq = 1;
  /** Highest seq the peer has acknowledged. */
  private _peerAck = 0;
  /** Highest seq we have received from the peer. */
  private _localAck = 0;
  /** Buffer of { seq, data } for unacked outbound messages. */
  private _buffer: Array<{ seq: number; data: Uint8Array }> = [];
  private _maxBuffer: number;

  constructor(maxBuffer = 10_000) {
    this._maxBuffer = maxBuffer;
  }

  /** Assign the next seq and current localAck to an outbound envelope. */
  stamp(envelope: EnvelopeMsg): number {
    const seq = this._nextSeq++;
    envelope.seq = seq;
    envelope.ack = this._localAck;
    return seq;
  }

  /** Store encoded bytes in the buffer for potential replay. */
  store(seq: number, data: Uint8Array): void {
    this._buffer.push({ seq, data });
    while (this._buffer.length > this._maxBuffer) {
      this._buffer.shift();
    }
  }

  /** Called when we receive a message from the peer. */
  onRecv(seq: number): void {
    if (seq > this._localAck) {
      this._localAck = seq;
    }
  }

  /** Called when we see the peer's ack field — remove acknowledged messages. */
  onPeerAck(ack: number): void {
    if (ack > this._peerAck) {
      this._peerAck = ack;
    }
    while (this._buffer.length > 0 && this._buffer[0].seq <= this._peerAck) {
      this._buffer.shift();
    }
  }

  /** After reconnect, get all unacked messages for replay. */
  drainUnacked(): Uint8Array[] {
    return this._buffer.map((entry) => entry.data);
  }

  /** The highest seq we received from the peer (for Hello.lastAck). */
  get localAck(): number {
    return this._localAck;
  }

  /** Number of buffered (unacked) messages. */
  get pendingCount(): number {
    return this._buffer.length;
  }
}

/** Stamp, encode, store in outbox, and return the encoded bytes. */
export function prepareOutbound(
  outbox: Outbox,
  envelope: EnvelopeMsg,
): Uint8Array {
  const seq = outbox.stamp(envelope);
  const data = encodeEnvelope(envelope);
  outbox.store(seq, data);
  return data;
}
