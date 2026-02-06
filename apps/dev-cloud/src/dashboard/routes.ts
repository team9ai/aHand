import { Hono } from "hono";
import { z } from "zod";
import { zValidator } from "@hono/zod-validator";
import type WebSocket from "ws";
import type { AHandServer } from "@ahand/sdk";
import type { DashboardBroadcaster } from "./broadcaster.ts";
import type { DashboardState } from "./state.ts";

// ── Zod schemas for POST bodies ──────────────────────────────────

const execSchema = z.object({
  tool: z.string(),
  args: z.array(z.string()).optional(),
  cwd: z.string().optional(),
  deviceId: z.string().optional(),
  env: z.record(z.string(), z.string()).optional(),
  timeout: z.number().optional(),
});

const cancelSchema = z.object({
  jobId: z.string(),
  deviceId: z.string().optional(),
});

const approveSchema = z.object({
  jobId: z.string(),
  approved: z.boolean(),
  remember: z.boolean().optional(),
  reason: z.string().optional(),
  deviceId: z.string().optional(),
});

const sessionModeSchema = z.object({
  deviceId: z.string().optional(),
  callerUid: z.string(),
  mode: z.enum(["inactive", "strict", "trust", "auto_accept"]),
  trustTimeoutMins: z.number().optional(),
});

const browserSchema = z.object({
  deviceId: z.string().optional(),
  sessionId: z.string(),
  action: z.string(),
  params: z.record(z.string(), z.unknown()).optional(),
  timeoutMs: z.number().optional(),
});

const policyUpdateSchema = z.object({
  addAllowedTools: z.array(z.string()).optional(),
  removeAllowedTools: z.array(z.string()).optional(),
  addDeniedTools: z.array(z.string()).optional(),
  removeDeniedTools: z.array(z.string()).optional(),
  addAllowedDomains: z.array(z.string()).optional(),
  removeAllowedDomains: z.array(z.string()).optional(),
  addDeniedPaths: z.array(z.string()).optional(),
  removeDeniedPaths: z.array(z.string()).optional(),
  approvalTimeoutSecs: z.number().optional(),
});

/**
 * Create dashboard API routes.
 * Routes are chained so we can export the type for Hono RPC.
 */
export function createDashboardRoutes(
  ahand: AHandServer,
  broadcaster: DashboardBroadcaster,
  state: DashboardState,
  upgradeWebSocket: (
    handler: (c: unknown) => {
      onOpen?: (evt: unknown, ws: { raw: unknown; }) => void;
      onClose?: () => void;
    },
  ) => unknown,
) {
  const routes = new Hono()
    // ── Dashboard WebSocket ────────────────────────────────────
    .get(
      "/dashboard/ws",
      upgradeWebSocket((_c: unknown) => ({
        onOpen: (_evt: unknown, ws: { raw: unknown; }) => {
          broadcaster.addClient(ws.raw as WebSocket);
        },
      })) as never,
    )

    // ── GET endpoints ──────────────────────────────────────────
    .get("/api/devices", (c) => {
      return c.json(ahand.devices().map((d) => d.toJSON()));
    })

    .get("/api/jobs", (c) => {
      return c.json(state.getActiveJobs());
    })

    .get("/api/approvals", (c) => {
      return c.json(state.getPendingApprovals());
    })

    .get("/api/events", (c) => {
      return c.json(state.getEventLog());
    })

    // ── POST /api/exec (non-blocking) ──────────────────────────
    .post("/api/exec", zValidator("json", execSchema), async (c) => {
      const body = c.req.valid("json");

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

      // Wire job events to broadcaster (non-blocking).
      broadcaster.wireJob(
        job.jobId,
        device.deviceId,
        body.tool,
        body.args ?? [],
        body.cwd ?? "",
        job,
      );

      return c.json({ jobId: job.jobId });
    })

    // ── POST /api/cancel ───────────────────────────────────────
    .post("/api/cancel", zValidator("json", cancelSchema), async (c) => {
      const body = c.req.valid("json");

      const device = body.deviceId
        ? ahand.device(body.deviceId)
        : ahand.devices()[0];

      if (!device) {
        return c.json({ error: "no device connected" }, 404);
      }

      console.log(`[routes] cancel jobId=${body.jobId} deviceId=${device.deviceId}`);
      device.cancelJob(body.jobId);
      return c.json({ ok: true, jobId: body.jobId });
    })

    // ── POST /api/approve ──────────────────────────────────────
    .post("/api/approve", zValidator("json", approveSchema), async (c) => {
      const body = c.req.valid("json");

      const device = body.deviceId
        ? ahand.device(body.deviceId)
        : ahand.devices()[0];

      if (!device) {
        return c.json({ error: "no device connected" }, 404);
      }

      device.approveJob(body.jobId, body.approved, body.remember ?? false, body.reason ?? "");
      state.resolveApproval(body.jobId);
      broadcaster.broadcast({
        type: "approval.resolved",
        ts: Date.now(),
        jobId: body.jobId,
        approved: body.approved,
      });

      return c.json({ ok: true, jobId: body.jobId });
    })

    // ── POST /api/session/mode ──────────────────────────────────
    .post("/api/session/mode", zValidator("json", sessionModeSchema), async (c) => {
      const body = c.req.valid("json");

      const device = body.deviceId
        ? ahand.device(body.deviceId)
        : ahand.devices()[0];

      if (!device) {
        return c.json({ error: "no device connected" }, 404);
      }

      const modeMap: Record<string, number> = {
        inactive: 0,
        strict: 1,
        trust: 2,
        auto_accept: 3,
      };

      device.setSessionMode(
        body.callerUid,
        modeMap[body.mode] ?? 0,
        body.trustTimeoutMins ?? 0,
      );

      return c.json({ ok: true, message: "session mode update sent" });
    })

    // ── GET /api/sessions ───────────────────────────────────────
    .get("/api/sessions", (c) => {
      const deviceId = c.req.query("deviceId");
      const device = deviceId ? ahand.device(deviceId) : ahand.devices()[0];
      if (!device) return c.json({ error: "no device connected" }, 404);

      device.querySession("");
      return c.json({ ok: true, message: "session query sent" });
    })

    // ── GET /api/policy/:deviceId ──────────────────────────────
    .get("/api/policy/:deviceId", (c) => {
      const deviceId = c.req.param("deviceId");
      const device = ahand.device(deviceId);

      if (!device) {
        return c.json({ error: "device not found" }, 404);
      }

      device.queryPolicy();
      return c.json({ ok: true, message: "policy query sent" });
    })

    // ── POST /api/policy/:deviceId ─────────────────────────────
    .post(
      "/api/policy/:deviceId",
      zValidator("json", policyUpdateSchema),
      async (c) => {
        const deviceId = c.req.param("deviceId");
        const device = ahand.device(deviceId);

        if (!device) {
          return c.json({ error: "device not found" }, 404);
        }

        const body = c.req.valid("json");

        device.updatePolicy({
          addAllowedTools: body.addAllowedTools ?? [],
          removeAllowedTools: body.removeAllowedTools ?? [],
          addDeniedTools: body.addDeniedTools ?? [],
          removeDeniedTools: body.removeDeniedTools ?? [],
          addAllowedDomains: body.addAllowedDomains ?? [],
          removeAllowedDomains: body.removeAllowedDomains ?? [],
          addDeniedPaths: body.addDeniedPaths ?? [],
          removeDeniedPaths: body.removeDeniedPaths ?? [],
          approvalTimeoutSecs: body.approvalTimeoutSecs ?? 0,
        });

        return c.json({ ok: true, message: "policy update sent" });
      },
    )

    // ── POST /api/browser ────────────────────────────────────────
    .post("/api/browser", zValidator("json", browserSchema), async (c) => {
      const body = c.req.valid("json");

      const device = body.deviceId
        ? ahand.device(body.deviceId)
        : ahand.devices()[0];

      if (!device) {
        return c.json({ success: false, error: "no device connected" }, 404);
      }

      try {
        const result = await device.browser(
          body.sessionId,
          body.action,
          body.params,
          { timeoutMs: body.timeoutMs },
        );
        return c.json(result);
      } catch (e: unknown) {
        const msg = e instanceof Error ? e.message : String(e);
        return c.json({ success: false, error: msg }, 500);
      }
    });

  return routes;
}

export type DashboardRoutes = ReturnType<typeof createDashboardRoutes>;
