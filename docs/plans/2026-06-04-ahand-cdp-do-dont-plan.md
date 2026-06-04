# aHand CDP Browser Runtime Do / Don't Plan

Date: 2026-06-04

## Purpose

This document narrows the cross-repository Chrome CDP browser plan down to the
aHand implementation boundary. It records what aHand should implement in phase
1, what it should intentionally not implement, and which behaviors need local
development validation.

Source contract:

- `docs/plans/2026-06-03-chrome-cdp-ahand-agent-pi-plan.md`

## Phase 1 Goal

aHand should provide one browser runtime contract that can be backed by either:

1. the existing Playwright / `playwright-cli` provider; or
2. a new CDP provider that can use the user's Chrome profile/session/cookie
   state.

Team9 and agent-pi should not need to know which provider executes a browser
job, except for passing Team9's selected provider through as execution metadata.
aHand owns final provider validation and execution.

## Do

### Configuration

- Extend the current flat `BrowserConfig`; do not migrate to nested provider
  config in phase 1.
- Add first-phase fields:
  - `selected_provider`
  - `cdp_enabled`
  - `cdp_mode`
  - `cdp_endpoint`
  - `playwright_enabled`
- Treat `[browser].enabled` as the top-level browser feature switch.
- Treat `playwright_enabled` and `cdp_enabled` as sibling provider gates.
- Treat `selected_provider` in aHand config as a daemon-local default and
  backward-compatibility value. For Team9-owned sessions, the per-job/session
  provider in `params_json.selected_provider` wins.

### Capability And Provider Status

- Report the stable `browser` capability through aHand Hub when browser
  automation is enabled and at least one provider is available for on-demand
  use.
- Keep accepting legacy `browser-playwright-cli` capability semantics during
  rollout where needed, but treat it only as an alias for `browser`.
- Expose provider availability through local browser status for Team9 desktop
  UI, for example:

```ts
{
  browserProviders: Array<"cdp" | "playwright">;
}
```

- Interpret `browserProviders` as "aHand can use this provider on demand", not
  "the backing browser process is currently running".
- Omit unavailable providers from `browserProviders`.
- Keep granular provider states such as installed/configured/enabled/diagnostic
  internal to aHand for phase 1.

### CDP Provider

- Implement a CDP provider that uses the user's Chrome profile.
- Support explicit local endpoint attach:
  - when `cdp_endpoint` is set, probe and connect to that endpoint;
  - do not close the external browser process when attached to an explicit
    endpoint.
- Support aHand-launched user-profile CDP Chrome:
  - when `cdp_endpoint` is empty, probe known aHand-owned CDP endpoints first;
  - if no known endpoint is alive, allocate an available localhost-only
    remote-debugging port;
  - launch Chrome with the user's profile and remote debugging enabled;
  - connect to the launched endpoint.
- Bind remote debugging only to localhost.
- Track whether a Chrome process was launched by aHand.
- Close aHand-launched Chrome during browser session or daemon lifecycle
  cleanup.

### Browser Job Contract

- Keep using the existing `BrowserRequest` / `BrowserResponse` transport.
- Keep `BrowserRequest.action` as the top-level action field.
- Do not duplicate the action inside `params_json`.
- Carry first-phase provider and CDP target metadata inside `params_json`:

```ts
type BrowserParamsJson = {
  selected_provider?: "playwright" | "cdp";
  target?: {
    mode?: "new_tab" | "session_tab";
  };
  url?: string;
  selector?: string;
  element_ref?: string;
  text?: string;
  expression?: string;
  timeout_ms?: number;
};
```

- Use existing `BrowserRequest.action` names in phase 1:
  - `open`
  - `click`
  - `fill`
  - `type`
  - `eval`
  - `wait`
- Implement the same action subset for Playwright and CDP providers where
  provider support exists.
- Expose `wait` through the standard `BrowserRequest` path by reusing the
  existing OpenClaw browser wait semantics:
  - text polling;
  - delay wait.

### CDP Target Lifecycle

- Support `target.mode = "new_tab"` first.
- Support `target.mode = "session_tab"` only for tabs aHand created and tracks
  in the same browser session.
- Track `session_id` to CDP target/page state so later actions in the same
  session can reuse the tab aHand created.
- Do not require agent-pi to pass CDP target ids in phase 1.
- Do not close the tab at the end of each browser job.
- Keep tabs available for later actions in the same browser session.

### Error Handling

- Validate provider availability at execution time.
- If `selected_provider` is unavailable, return an error through existing
  aHand job/browser response fields.
- Do not silently fall back from CDP to Playwright, or from Playwright to CDP,
  once a browser job reaches aHand with an explicit provider.
- Reuse existing aHand job/browser response error fields for:
  - provider unavailable;
  - CDP startup failure;
  - profile lock;
  - target lost;
  - timeout.

## Don't Do

- Do not introduce nested browser provider config in phase 1.
- Do not add new `BrowserRequest.action` names for phase 1.
- Do not make CDP expose provider-specific action names.
- Do not require agent-pi to send CDP target ids.
- Do not support arbitrary `active_tab` control.
- Do not attach to or operate on unrelated user tabs that aHand did not create
  or track.
- Do not add a new screenshot or binary artifact contract in phase 1.
- Do not require screenshot support for phase 1 CDP delivery.
- Do not expose provider-specific readiness through Hub/Gateway in phase 1.
- Do not persist `browserProviders` in Hub/Gateway-owned server state.
- Do not infer provider availability from legacy capability aliases.
- Do not let aHand choose a different provider after Team9/agent-pi passed an
  explicit `selected_provider`.
- Do not implement extra security approval dialogs in phase 1.
- Do not bind Chrome remote debugging publicly.
- Do not close external Chrome processes attached through explicit
  `cdp_endpoint`.

## Development Validation

These are implementation risks aHand should verify locally before declaring the
CDP provider ready:

- User's normal Chrome is already running without remote debugging and
  `cdp_endpoint` is empty.
  - Expected phase-1 outcome: handle reliably if possible; otherwise return a
    clear existing browser response error.
- User profile lock or Chrome startup failure.
- Automatic localhost port allocation failure.
- aHand-owned endpoint registry behavior after daemon restart.
- Session cleanup closes only aHand-launched Chrome, not explicit external
  endpoint Chrome.
- `wait` behavior works through the standard `BrowserRequest` path, not only
  through OpenClaw browser routing.

## Tests

Add or update aHand tests for:

- Flat `BrowserConfig` parsing with the new provider fields.
- `browser` capability is reported when browser automation is enabled and at
  least one provider is available for on-demand use.
- Local provider status reports:
  - Playwright-only setup as `["playwright"]`;
  - CDP-only setup as `["cdp"]`;
  - mixed setup as `["cdp", "playwright"]`.
- Legacy `browser-playwright-cli` remains only compatibility behavior and does
  not imply provider availability.
- `params_json.selected_provider` wins over daemon-local `selected_provider`
  for Team9-owned browser jobs.
- Explicit selected provider unavailable returns an error and does not fall
  back.
- CDP explicit endpoint attach succeeds.
- CDP empty endpoint path launches user-profile Chrome with an automatically
  allocated localhost-only remote-debugging port.
- CDP `new_tab` creates and tracks a tab.
- CDP `session_tab` reuses the tracked tab for the same `session_id`.
- `BrowserRequest.action` support for:
  - `open`;
  - `click`;
  - `fill`;
  - `type`;
  - `eval`;
  - `wait` text polling;
  - `wait` delay.
- aHand-launched Chrome is closed during cleanup.
- Explicit external endpoint Chrome is not closed by aHand.

## Phase 1 Acceptance Criteria

- Team9 can call local browser status and see provider availability.
- aHand Hub reports the stable `browser` capability.
- agent-pi can send `BrowserRequest` jobs using the existing action subset and
  `params_json.selected_provider`.
- CDP jobs can use the user's Chrome profile/session/cookie state.
- Provider choice is validated by aHand at execution time.
- No new BrowserRequest action names, screenshot artifact contract, arbitrary
  active-tab control, or provider-specific Hub/Gateway metadata is required for
  phase 1.
