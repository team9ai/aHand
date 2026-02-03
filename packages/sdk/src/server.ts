import { EventEmitter } from "node:events";
import type WebSocket from "ws";
import type { Envelope as EnvelopeMsg } from "@ahand/proto";
import { decodeEnvelope } from "./codec.ts";
import { DeviceConnection } from "./connection.ts";
import { Outbox } from "./outbox.ts";

export class AHandServer extends EventEmitter {
  private readonly _devices = new Map<string, DeviceConnection>();

  /** Call this when a raw WebSocket connection is established. */
  handleSocket(ws: WebSocket): void {
    // Wait for the first message which must be a Hello.
    const onFirstMessage = (raw: Buffer) => {
      ws.off("message", onFirstMessage);

      let envelope: EnvelopeMsg;
      try {
        envelope = decodeEnvelope(new Uint8Array(raw));
      } catch {
        ws.close(1002, "invalid protobuf");
        return;
      }

      if (!envelope.hello) {
        ws.close(1002, "first message must be Hello");
        return;
      }

      const deviceId = envelope.deviceId;
      const hello = envelope.hello;

      // On reconnect: transfer the outbox from the old connection.
      let outbox: Outbox | undefined;
      const existing = this._devices.get(deviceId);
      if (existing) {
        outbox = existing.outbox;

        // The daemon's Hello carries last_ack — clear acknowledged messages.
        if (hello.lastAck > 0) {
          outbox.onPeerAck(hello.lastAck);
        }

        existing.close();
      }

      const conn = new DeviceConnection(deviceId, hello, ws, outbox);

      // Replay unacked messages to the new connection.
      if (outbox) {
        const unacked = outbox.drainUnacked();
        for (const data of unacked) {
          ws.send(data);
        }
      }

      this._devices.set(deviceId, conn);

      ws.on("close", () => {
        // Only remove if this is still the active connection for this device.
        if (this._devices.get(deviceId) === conn) {
          this._devices.delete(deviceId);
          this.emit("deviceDisconnected", conn);
        }
      });

      this.emit("device", conn);
    };

    ws.on("message", onFirstMessage);
  }

  /** List all connected devices. */
  devices(): DeviceConnection[] {
    return [...this._devices.values()];
  }

  /** Get a device by ID. */
  device(deviceId: string): DeviceConnection | undefined {
    return this._devices.get(deviceId);
  }

  /** Register a callback for when a device connects. */
  onDevice(callback: (conn: DeviceConnection) => void): void {
    this.on("device", callback);
  }
}
