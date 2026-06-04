# Chrome DevTools CDP Capability Plan

Date: 2026-06-03

## Purpose

This document records the plan for adding Chrome DevTools Protocol (CDP)
capability to Team9's desktop client and making it available to agents through
the existing aHand and agent-pi integration.

The core product requirement is not just "browser automation". The requirement
is that agent browser calls can use an existing Chrome session/profile/cookie
state when the user explicitly enables that mode. A dedicated Playwright
automation profile is sufficient for generic browser automation, but it does
not satisfy "use my already logged-in Chrome state" unless that state lives in
the same managed profile. Reusing an already-running Chrome session requires a
CDP attach or managed CDP Chrome flow.

The intended audience is the Team9 client/server team, the aHand team, and the
agent-pi team. The goal is to align on ownership, contracts, and rollout order
before implementation.

## Current Understanding

Team9 does not call agent-pi through aHand by default. The normal agent path is:

```text
Team9 client
  -> Team9 Gateway / WebSocket / REST
  -> im-worker or gateway service
  -> ClawHiveService
  -> agent-pi / claw-hive
```

aHand is an optional local-device execution backend injected into agent-pi
sessions. It is used when an agent needs to operate on the user's local machine,
for example shell execution or browser automation.

The current aHand path is:

```text
Team9 client Tauri
  -> embedded ahandd
  -> aHand Hub WebSocket
  -> agent-pi ahand-host component
  -> local job execution on user's machine
```

Team9 already has a browser runtime setup surface in the Tauri client:

- `browser_status`
- `browser_install`
- `browser_set_enabled`

Those commands live in:

- `apps/client/src-tauri/src/ahand/browser_runtime.rs`
- `apps/client/src-tauri/src/ahand/runtime.rs`

Today this setup mainly installs/checks browser runtime prerequisites, writes
`~/.ahand/config.toml`, toggles `[browser].enabled`, and reloads the embedded
`ahandd`.

aHand already has a Playwright-backed browser path:

- `ahandd` reports the `browser-playwright-cli` capability when browser support
  is enabled and available.
- `BrowserRequest` / `BrowserResponse` already carry browser action requests
  and binary screenshot/PDF results.
- `ahandd` currently executes browser commands through `playwright-cli`.

That existing path is useful and should remain, but it is not the same as CDP
session reuse. This plan separates generic browser automation from CDP-backed
session/profile reuse.

## Desired End State

An agent-pi session can discover that a user's local device supports browser
automation and, when explicitly enabled, browser automation against an existing
or managed CDP Chrome session. It can expose appropriate browser tools to the
model, execute those tools through aHand on the user's machine, and stream
results back to Team9.

There are two browser modes:

1. **Managed Playwright browser**: aHand uses Playwright / `playwright-cli` and
   an aHand-managed browser profile. This is suitable for normal browser
   automation and can persist cookies inside that managed profile.
2. **CDP session reuse**: aHand connects to a Chrome DevTools endpoint for a
   Chrome process that was launched with remote debugging, or launches/manages
   such a Chrome process. This is the mode required when the user wants agents
   to operate with an existing Chrome profile/session/cookie state.

At a high level:

```text
User enables browser and optionally CDP session reuse in Team9 desktop client
  -> embedded ahandd reports capability to aHand Hub
  -> aHand Hub webhooks capabilities to Team9 Gateway
  -> Team9 persists device capabilities
  -> im-worker injects ahand-host with capabilities into agent-pi session
  -> agent-pi registers browser/CDP tools for that session
  -> tool calls execute through aHand Hub on local ahandd
  -> results stream back through existing job/session event paths
```

## Proposed Capability Names

This needs final agreement with aHand and agent-pi.

Recommended minimum for generic browser automation:

```json
["exec", "browser"]
```

Recommended full set when aHand supports both Playwright and CDP attach/managed
CDP:

```json
["exec", "browser", "browser.playwright", "browser.cdp"]
```

Capability semantics:

- `browser`: broad browser automation is available. agent-pi may expose generic
  browser tools.
- `browser.playwright`: browser automation is backed by aHand's Playwright /
  `playwright-cli` provider.
- `browser.cdp`: browser automation can use Chrome DevTools Protocol against an
  existing or managed CDP Chrome endpoint, and can therefore reuse the configured
  Chrome session/profile/cookie state.

Do not report `browser.cdp` for a pure Playwright provider. The point of
`browser.cdp` is to let agent-pi know it may rely on CDP session/profile reuse
semantics.

## Team9 Responsibilities

### 1. Client Tauri Runtime Setup

Files:

- `apps/client/src-tauri/src/ahand/browser_runtime.rs`
- `apps/client/src-tauri/src/ahand/runtime.rs`
- `apps/client/src/hooks/useBrowserRuntime.ts`

Planned work:

- Keep using the existing Tauri command surface for browser runtime setup.
- Extend browser status if needed to include CDP-specific readiness.
- Confirm or add config fields needed by `ahandd`, for example:

```toml
[browser]
enabled = true
provider = "playwright"

[browser.cdp]
cdp_enabled = true
mode = "attach_existing"
endpoint = "http://127.0.0.1:9222"
```

Open questions:

- Does Team9 write CDP config fields directly, or does it call aHand setup APIs
  that persist them?
- Should Team9 expose an "attach to existing Chrome" mode that instructs users
  to launch Chrome with remote debugging?
- Should Team9 expose a "managed CDP Chrome" mode where aHand launches Chrome
  with an isolated or selected profile?
- Should Team9 expose Chrome path/profile selection in the UI, or only use
  aHand defaults?

### 2. Client UI

Files:

- `apps/client/src/components/dialog/devices/ThisMacSection.tsx`
- `apps/client/src/hooks/useBrowserRuntime.ts`
- `apps/client/src/services/ahand-api.ts`

Planned work:

- Show browser readiness and CDP session-reuse readiness in the local
  device/browser setup UI.
- Optionally show device capabilities returned by the Gateway:
  `exec`, `browser`, `browser.playwright`, `browser.cdp`.
- Keep the UI as a setup/status surface, not as a direct CDP client.

Non-goal:

- The React app should not directly connect to a Chrome CDP websocket for agent
  tool execution. Local execution should stay behind `ahandd`.

### 3. Gateway Device API and Capability Persistence

Files:

- `apps/server/apps/gateway/src/ahand/ahand.service.ts`
- `apps/server/apps/gateway/src/ahand/ahand-webhook.service.ts`
- `apps/server/apps/gateway/src/ahand/dto/device.dto.ts`
- `apps/server/apps/gateway/src/ahand/dto/internal.dto.ts`
- `apps/server/libs/database/src/schemas/im/ahand-devices.ts`

Current state:

- `im_ahand_devices.capabilities` already exists.
- `AhandWebhookService` already persists capabilities from `device.online` and
  `device.registered` webhook data.
- Gateway internal DTO already includes `capabilities`.
- Public `DeviceDto` currently does not expose `capabilities`.

Planned work:

- Add `capabilities: string[]` to public `DeviceDto` if the client UI needs to
  display them.
- Ensure `toDeviceDto()` includes `row.capabilities ?? []`.
- Keep existing webhook semantics:
  - `device.online` capabilities are authoritative.
  - `device.registered` with empty capabilities should not wipe a later online
    capabilities value.

### 4. im-worker Capability Propagation

Files:

- `apps/server/apps/im-worker/src/ahand/ahand-control-plane.client.ts`
- `apps/server/apps/im-worker/src/ahand/ahand-blueprint.extender.ts`

Current gap:

Gateway internal endpoints return `capabilities`, but the im-worker Zod schema
does not parse that field yet. `AhandBlueprintExtender` also does not pass
capabilities into `ahand-host.config`.

Planned work:

- Extend `DeviceSchema`:

```ts
capabilities: z.array(z.string()).default([])
```

- Extend `AhandDeviceSummary` usage accordingly.
- Pass capabilities into `ahand-host` config:

```ts
config: {
  deviceId: d.hubDeviceId,
  deviceNickname: d.nickname,
  devicePlatform: d.platform,
  capabilities: d.capabilities,
  callingUserId: input.callingUserId,
  callingClient,
  gatewayInternalUrl: this.gatewayInternalUrl,
  gatewayInternalAuthToken: token,
  hubUrl: this.hubUrl,
}
```

Expected result:

- agent-pi can make per-session tool registration decisions based on the
  actual online device capabilities.

## aHand Responsibilities

Repository outside Team9:

- `https://github.com/team9ai/aHand`
- Team9 currently depends on `ahandd` tag `rust-v0.1.13`.

Planned work:

- Implement or confirm Chrome/CDP execution support in `ahandd`.
- Define the job protocol for browser/CDP actions.
- Report provider-specific capabilities:
  - `browser.playwright` for the existing Playwright provider.
  - `browser.cdp` only when CDP attach/managed Chrome is configured and usable.
- Own Chrome process/profile/debug-port management for managed CDP mode, unless
  explicitly delegated to Team9 Tauri setup.
- Support an attach-existing CDP mode for explicit endpoints such as
  `http://127.0.0.1:9222`.
- Report browser/CDP readiness through aHand Hub capabilities.
- Stream job output in the existing aHand job event format.

Open questions:

- Does `ahandd` launch Chrome itself, connect to an existing Chrome, or support
  both? Recommendation: support both, with managed CDP first and attach-existing
  second.
- How are ports chosen and protected?
- Is a dedicated browser profile required for managed mode?
- How should a user opt into reusing an everyday Chrome profile, and what
  warnings/approvals are required?
- How are screenshots or binary artifacts returned?
- What are the exact job payload schemas for CDP actions?

## agent-pi Responsibilities

Repository outside Team9:

- `team9-agent-pi` / claw-hive runtime

Planned work:

- Update the `ahand-host` component to consume `config.capabilities`.
- Register browser tools only when the target aHand device supports the
  required capability.
- Register session-reuse dependent tools only when the device reports
  `browser.cdp`.
- Define model-facing tools, for example:
  - `browser_navigate`
  - `browser_click`
  - `browser_type`
  - `browser_evaluate`
  - `browser_screenshot`
  - `browser_wait_for`
- Route browser/CDP tool calls through aHand Hub jobs to the selected device.
- Normalize tool call/result events so Team9 can display them with existing
  tool event UI.

Open questions:

- Should browser tools target the calling user's current device by default?
- If multiple devices are online, how is the device selected?
- Should tool names be generic browser automation names or explicit CDP names?
- What error shape should be returned when browser capability is unavailable?
- Should agent-pi expose profile/session-reuse state to the model, or keep it
  hidden and only expose a generic browser tool surface?

## Contract Points

### Device Capabilities

Source of truth:

- `ahandd` declares capabilities to aHand Hub.
- aHand Hub forwards capabilities in device webhooks.
- Team9 Gateway persists capabilities in `im_ahand_devices.capabilities`.
- im-worker passes capabilities into agent-pi session component config.

Suggested data shape:

```json
{
  "capabilities": ["exec", "browser", "browser.playwright", "browser.cdp"]
}
```

### ahand-host Component Config

Proposed Team9-to-agent-pi config:

```json
{
  "deviceId": "hub-device-id",
  "deviceNickname": "macos-device",
  "devicePlatform": "macos",
  "capabilities": ["exec", "browser", "browser.playwright", "browser.cdp"],
  "callingUserId": "team9-user-id",
  "callingClient": {
    "kind": "macapp",
    "deviceId": "hub-device-id",
    "deviceNickname": "macos-device",
    "isAhandEnabled": true
  },
  "gatewayInternalUrl": "https://...",
  "gatewayInternalAuthToken": "...",
  "hubUrl": "https://..."
}
```

### Tool Results

Preferred behavior:

- Browser/CDP tool calls should appear in Team9 as normal agent tool events.
- Streaming progress can use existing aHand job stream where appropriate.
- Final results should include concise text plus structured data when useful.
- Screenshots should use an artifact or image block shape that Team9 already
  supports in tool results, if available.

### Browser Job Payloads

The existing aHand `BrowserRequest` shape can remain the transport envelope:

```json
{
  "session_id": "agent-browser-session",
  "action": "goto",
  "params_json": "{\"url\":\"https://example.com\"}",
  "timeout_ms": 30000
}
```

For provider selection, add one of:

```json
{
  "provider": "playwright"
}
```

```json
{
  "provider": "cdp",
  "target": {
    "mode": "active_tab"
  }
}
```

or:

```json
{
  "provider": "cdp",
  "target": {
    "mode": "new_tab",
    "url": "https://example.com"
  }
}
```

If the transport cannot be changed immediately, provider selection can live in
`params_json` during the first implementation. The long-term contract should
make provider/target explicit.

## Implementation Order

### Phase 1: Contract Alignment

- Agree on capability names.
- Agree that `browser.cdp` specifically means CDP-backed session/profile/cookie
  reuse, not just generic browser automation.
- Agree on browser/CDP job payloads.
- Agree on tool names and result shapes.
- Decide whether Team9 UI needs to expose capabilities or only status.

### Phase 2: Team9 Capability Plumbing

- Add public `DeviceDto.capabilities` if needed.
- Add im-worker `capabilities` parsing.
- Pass capabilities into `ahand-host.config`.
- Add tests for Gateway DTO and im-worker injection.

### Phase 3: aHand Browser/CDP Runtime

- Implement or update `ahandd` CDP support.
- Ensure daemon hello reports browser/CDP capability only when ready.
- Ensure `browser_install` / `browser_set_enabled` config is sufficient.
- Implement a CDP provider that can:
  - connect to an explicit local CDP endpoint;
  - enumerate tabs/targets;
  - navigate, click, type, evaluate, wait, and screenshot;
  - reuse the endpoint's browser profile/session/cookie state.
- Keep the existing Playwright provider and report it separately as
  `browser.playwright`.
- Publish a new `ahandd` tag.
- Update Team9 `Cargo.toml` to the new `ahandd` tag.

### Phase 4: agent-pi Tool Registration

- Update `ahand-host` to register browser tools based on capabilities.
- Implement tool routing to aHand jobs.
- Normalize tool event metadata.
- Add agent-pi tests for capability-gated tool availability.

### Phase 5: End-to-End Verification

Verify the full path:

1. Enable local device in Team9 desktop.
2. Install/enable browser runtime.
3. Confirm `ahandd` online.
4. Confirm aHand Hub emits capabilities.
5. Confirm Gateway stores capabilities.
6. Confirm im-worker injects `ahand-host` with capabilities.
7. Start an agent-pi session.
8. Confirm browser tools are available to the agent.
9. Run a simple Playwright browser task:
   - open a page
   - read title
   - take screenshot
10. Run a CDP session-reuse task:
   - start or attach Chrome with remote debugging;
   - log into a site in that Chrome profile;
   - ask the agent to open/read the already-authenticated page;
   - confirm the agent sees authenticated state without re-login.
11. Confirm Team9 UI displays tool call and result.

## Testing Plan

Team9 tests:

- Gateway:
  - `DeviceDto` includes capabilities.
  - webhook persists `browser` / `browser.cdp`.
  - internal endpoint still returns capabilities.
- im-worker:
  - `AhandControlPlaneClient` parses capabilities.
  - `AhandBlueprintExtender` injects capabilities into `ahand-host`.
  - missing capabilities defaults to `[]`.
- Client:
  - `ahandApi.DeviceDto` includes capabilities if exposed.
  - browser runtime UI handles CDP-ready and CDP-not-ready states if added.

aHand tests:

- daemon reports capability only when browser/CDP is configured and usable.
- CDP job succeeds against a local Chrome instance.
- CDP job can reuse a configured profile/session/cookie state.
- pure Playwright setup does not report `browser.cdp`.
- failure cases: Chrome missing, port conflict, profile locked, timeout.

agent-pi tests:

- browser tools unavailable when capability missing.
- generic browser tools available when `browser` is present.
- CDP session-reuse dependent tools available only when `browser.cdp` is
  present.
- tool call routes to the selected aHand device.
- tool errors are surfaced as normal tool results.

## Risks

- Capability drift: Team9, aHand, and agent-pi must use the same capability
  names.
- Partial rollout: Team9 may pass capabilities before agent-pi consumes them.
- Multiple online devices: agent-pi needs a deterministic target selection
  rule.
- Security: CDP can access sensitive browser state if attached to a user's
  everyday Chrome profile. This mode must be explicitly enabled and clearly
  surfaced to the user.
- Port exposure: Chrome remote debugging must not bind publicly.
- Profile safety: launching a second Chrome against a locked everyday profile
  can fail or risk profile corruption. Attach-existing should require a running
  remote-debugging Chrome; managed mode should prefer an isolated profile.
- Version skew: Team9 pins `ahandd` by git tag, so a new aHand release is
  required for Team9 to consume daemon changes.

## Non-Goals

- React client directly connecting to Chrome CDP.
- Replacing agent-pi's normal tool system with aHand.
- Routing all Team9 agent traffic through aHand.
- Building a second Team9-native local job protocol parallel to aHand.

## Immediate Next Steps

1. Team9 and aHand agree on browser/CDP config and capability names.
2. Team9 and agent-pi agree on `ahand-host.config.capabilities` and tool names.
3. Team9 and aHand agree that `browser.cdp` means session/profile/cookie reuse
   through CDP attach or managed CDP Chrome.
4. Team9 implements im-worker capability passthrough.
5. aHand publishes an `ahandd` build that reports and executes Playwright and
   CDP providers separately.
6. agent-pi gates browser tools on capabilities and routes calls through aHand.
