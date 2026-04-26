# Contracts

JSON Schema (Draft 2020-12) definitions for the wire formats that cross repository boundaries. Each schema is the single source of truth for one cross-repo contract; downstream services validate against the file, not the Rust types directly.

## Files

| File | What it pins | Producer | Consumer |
| --- | --- | --- | --- |
| `hub-webhook.json` | The JSON envelope the hub POSTs to a downstream gateway on device lifecycle events. | `crates/ahand-hub/src/webhook/mod.rs::WebhookPayload` (Rust serde) | `team9/apps/server/apps/gateway/src/ahand/dto/webhook-event.dto.ts` (NestJS class-validator DTO) |
| `hub-control-plane.json` | Request bodies, success responses, error envelopes, and SSE event payloads of `/api/control/jobs*`. | `crates/ahand-hub/src/http/control_plane.rs` (Rust axum handlers) | `packages/sdk/src/cloud-client.ts::CloudClient` (TypeScript SDK) |

## Stability rules

A schema change is **breaking** (and downstream consumers must be updated in lockstep, or the change must be staged behind a version bump) when it:

- adds a new required field;
- tightens an existing field's type, format, or range;
- removes or renames an existing enum value or property;
- toggles `additionalProperties` from `true`/absent to `false`.

A schema change is **additive** (safe) when it:

- adds an optional property to a structure that already had `additionalProperties: true` or implicit;
- widens an enum (consumers must accept unknown values gracefully — the gateway DTO already does for non-listed `eventType`s by rejecting them at the validator, while the SDK ignores unknown SSE event names by design);
- relaxes a `minLength`/`pattern` constraint.

## Verification

Each repository owns a thin contract test that loads the JSON Schema, validates the canonical happy-path payload, and asserts that the producer/consumer can round-trip through it:

- **`team9/apps/server/apps/gateway/test/contracts/hub-webhook.contract.e2e-spec.ts`** — bootstraps the webhook controller, signs a canonical payload, asserts 204; also exercises rejection paths against fuzz inputs the schema marks invalid.
- **`team9-agent-pi/packages/sdk-contracts/src/cloud-client.contract.test-d.ts`** — `tsd` type-level locks for `CloudClient.spawn`, `CloudClient.cancel`, and `CloudClient`'s exported types so a refactor of `@ahandai/sdk` cannot silently drift the consumer side.
- **`team9/k6/ahand-load-baseline.js`** — feeds the gateway a stream of schema-conforming webhook payloads under load, with payloads pre-signed by `team9/k6/scripts/gen-webhook-payloads.mjs`.

When a downstream consumer's contract test fails after a hub change, the hub author is expected to re-publish the schema before merging — that's the gating signal.

## Versioning

The schemas use `$id` URIs (`https://ahand.team9.ai/contracts/<name>.json`) but are NOT served from that URL. The URI is purely an identifier for `$ref` resolution and tooling; downstream consumers vendor the file via a git path or CI-downloaded artifact.

If a breaking change ever requires concurrent old + new shapes in the wild, append a `-v2` suffix to the file name and `$id` rather than mutating the existing file.
