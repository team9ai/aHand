import { Hono } from "hono";
import { serve } from "@hono/node-server";
import { createNodeWebSocket } from "@hono/node-ws";
import type WebSocket from "ws";
import { AHandServer } from "@ahand/sdk";

const app = new Hono();
const ahand = new AHandServer();

const { injectWebSocket, upgradeWebSocket } = createNodeWebSocket({ app });

// ── WebSocket endpoint ──────────────────────────────────────────────
app.get(
  "/ws",
  upgradeWebSocket((_c) => ({
    onOpen: (_evt, ws) => {
      // Hand the raw ws.WebSocket to the SDK for protobuf handling.
      if (ws.raw) {
        ahand.handleSocket(ws.raw as WebSocket);
      }
    },
  })),
);

// ── REST API ────────────────────────────────────────────────────────
app.get("/devices", (c) => {
  return c.json(ahand.devices().map((d) => d.toJSON()));
});

app.post("/exec", async (c) => {
  const body = await c.req.json<{
    deviceId?: string;
    tool: string;
    args?: string[];
    cwd?: string;
    env?: Record<string, string>;
    timeout?: number;
  }>();

  // If no deviceId, pick the first connected device.
  const device = body.deviceId
    ? ahand.device(body.deviceId)
    : ahand.devices()[0];

  if (!device) {
    return c.json({ error: "no device connected" }, 404);
  }

  const job = device.exec(body.tool, body.args ?? [], {
    cwd: body.cwd,
    env: body.env,
    timeoutMs: body.timeout,
  });

  const result = await job.done;
  return c.json({ jobId: job.jobId, ...result });
});

app.post("/cancel", async (c) => {
  const body = await c.req.json<{
    deviceId?: string;
    jobId: string;
  }>();

  const device = body.deviceId
    ? ahand.device(body.deviceId)
    : ahand.devices()[0];

  if (!device) {
    return c.json({ error: "no device connected" }, 404);
  }

  device.cancelJob(body.jobId);
  return c.json({ ok: true, jobId: body.jobId });
});

// ── SDK events ──────────────────────────────────────────────────────
ahand.onDevice((conn) => {
  console.log(
    `[device] connected: ${conn.hostname} (${conn.deviceId}) os=${conn.os}`,
  );
});

ahand.on("deviceDisconnected", (conn) => {
  console.log(`[device] disconnected: ${conn.hostname} (${conn.deviceId})`);
});

// ── Start server ────────────────────────────────────────────────────
const PORT = Number(process.env.PORT) || 3000;

const server = serve({ fetch: app.fetch, port: PORT }, () => {
  console.log(`dev-cloud listening on http://localhost:${PORT}`);
  console.log(`  WebSocket: ws://localhost:${PORT}/ws`);
  console.log(`  Devices:   http://localhost:${PORT}/devices`);
  console.log(`  Exec:      POST http://localhost:${PORT}/exec`);
  console.log(`  Cancel:    POST http://localhost:${PORT}/cancel`);
});

injectWebSocket(server);
