# @ahandai/proto Changelog

## 0.2.0 — 2026-04-30

Released alongside `@ahandai/sdk@0.2.0`. Both packages are now published
via CI (`.github/workflows/release-sdk.yml`) on a `release-v<semver>` tag
push, using npm trusted-publisher OIDC (no long-lived token).

### Added

- **`FileRequest` / `FileResponse`** wire types for the 14 file operations
  defined in `proto/ahand/v1/file_ops.proto`: `stat`, `list`, `glob`,
  `read_text`, `read_binary`, `read_image`, `write`, `edit`, `delete`,
  `chmod`, `mkdir`, `copy`, `move`, `create_symlink`. Consumed by the hub
  for dashboard / control-plane file operations and by the daemon's
  `file_manager`. Generated via `ts-proto` from the upstream proto source;
  no hand-written code.

### Notes

- `@ahandai/sdk@0.2.0`'s `CloudClient.files()` does **not** depend on
  these types directly — it uses the JSON control-plane endpoint
  (`POST /api/control/files`). These generated types are provided for
  consumers that interact with the raw protobuf envelope (e.g. in-process
  hub integrations).

## 0.1.4 — 2026-04-24

Last manually-published version. See git history for earlier changes:
- `chore(sdk): rename npm scope @ahand/{sdk,proto} → @ahandai/{sdk,proto}` (#5)
- `fix(proto,sdk): emit .js suffix in generated imports` (#6)
- `fix(sdk,proto): add "type": "module"` (#7)
