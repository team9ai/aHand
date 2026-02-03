# AHand

Local execution gateway for cloud AI. Lets cloud-side orchestrators run tools on local machines behind NAT/firewalls via WebSocket, with strong typing (protobuf) and local policy enforcement.

## Architecture

```
Cloud (WS server)  ←──  WebSocket (protobuf)  ──→  Local daemon (WS client)
      │                                                    │
  @ahand/sdk                                           ahandd
  (control plane)                                  (job executor)
```

- **Cloud** hosts the WebSocket endpoint; local daemon connects outbound (no public IP needed).
- **SDK** accepts upgraded WS connections and provides a typed Job API.
- **ahandd** enforces local security policy before executing any job.

## Repository Structure

```
ahand/
├─ proto/ahand/v1/          # Protobuf definitions (single source of truth)
├─ packages/
│  ├─ proto-ts/             # @ahand/proto — ts-proto generated types
│  └─ sdk/                  # @ahand/sdk — cloud control plane SDK
├─ apps/
│  └─ dev-cloud/            # Development cloud server (WS + dashboard)
├─ crates/
│  ├─ ahand-protocol/       # Rust prost generated types
│  ├─ ahandd/               # Local daemon (bin)
│  └─ ahandctl/             # CLI debug tool (bin)
├─ turbo.json               # Turborepo pipeline
├─ Cargo.toml               # Rust workspace
└─ pnpm-workspace.yaml      # pnpm monorepo
```

## Development

```bash
pnpm install        # install TS dependencies
pnpm build          # build all TS packages (turbo)
cargo check         # check Rust workspace
```

## License

Apache-2.0
