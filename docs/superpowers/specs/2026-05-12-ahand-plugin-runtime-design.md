# AHand Plugin Runtime Design

**Date:** 2026-05-12
**Status:** Approved design for planning

## Overview

AHand should upgrade its current hard-coded capability and dependency setup into a first-party plugin system. A plugin can provide host capabilities, managed runtimes, exported resource paths, setup/doctor behavior, and a short help prompt for agents. Plugins can depend on other plugins.

The first plugin set is:

```text
shell
node
python
file
browser-playwright-cli -> shell, node
```

The implementation should land in stages. First, replace the current `browser_setup` hard-coded `node -> playwright` installation flow with plugin lifecycle plumbing. Then migrate capability routing so `shell`, `file`, and `browser-playwright-cli` are registered through the plugin registry rather than being special daemon branches.

## Goals

- Make aHand host capabilities installable, inspectable, and discoverable as plugins.
- Keep runtime dependencies separate from project dependencies.
- Let plugins depend on other plugins, with explicit dependency resolution and failure reporting.
- Expose installed plugin state through a read-only `getHostResource` API.
- Preserve current daemon policy, approval, audit, and protocol behavior during migration.
- Avoid making `node` or `python` privileged concepts in the daemon. They are first-party runtime plugins.

## Non-Goals

- Third-party plugin marketplace support in the first implementation.
- Dynamic native code loading into the daemon process.
- Replacing the wire protocol in one large migration.
- Automatically installing missing plugins as a side effect of `getHostResource`.
- Moving project-specific dependencies into the aHand runtime cache.

## Plugin Boundaries

### `shell`

`shell` is both a capability plugin and a dependency plugin.

It provides:

- Non-interactive command execution.
- PTY execution.
- stdin forwarding.
- terminal resize handling.
- cwd and env propagation.
- stdout/stderr streaming.

It is the foundation for tools that need process execution semantics. `browser-playwright-cli` depends on it because browser commands are executed as managed subprocesses.

### `node`

`node` is a runtime plugin.

It provides:

- Managed Node.js installation.
- Managed npm path.
- Node/npm version inspection.
- exported executable resources for `node` and `npm`.

It should be bootstrap-installed by daemon-owned Rust installer code, not by shell scripts. This avoids dependency cycles where installing the shell plugin would require the shell plugin.

### `python`

`python` is a runtime plugin.

It provides:

- Managed Python installation.
- Python version inspection.
- exported executable resource for `python`.
- optional package directory resource when a bundled package set exists.

No current first-stage capability depends on Python, but defining it early keeps parity with the expected primary runtime model.

### `file`

`file` is a capability plugin.

It provides the existing file operation capability and owns file policy metadata:

- path allowlist.
- path denylist.
- dangerous path approval escalation.
- read/write byte limits.
- operation-level risk classification.

It does not depend on `shell`, `node`, or `python`.

### `browser-playwright-cli`

`browser-playwright-cli` is a capability plugin with runtime dependencies.

It depends on:

```text
shell
node
```

It provides:

- browser automation capability.
- playwright-cli inspection and repair.
- browser command execution through `playwright-cli`.
- exported `playwright-cli` executable resource.
- browser-specific help prompt.

The current `browser_setup::playwright` logic should move behind this plugin lifecycle while preserving the existing public CLI behavior during migration.

## Plugin Manifest

Each plugin has a TOML manifest owned by the plugin package. TOML matches the daemon's existing configuration style and stays easy to inspect manually.

```toml
id = "browser-playwright-cli"
version = "0.1.0"
display_name = "Browser Playwright CLI"

dependencies = ["shell", "node"]
capabilities = ["browser"]

[resources.executables.playwrightCli]
name = "playwright-cli"
path = "bin/playwright-cli"

[help]
prompt = "Provides browser automation through playwright-cli. Use for browser open, click, fill, snapshot, screenshot, PDF, download, and close actions."
```

Runtime plugins use the same shape:

```toml
id = "node"
version = "0.1.0"
display_name = "Node.js Runtime"

dependencies = []
capabilities = []

[resources.executables.node]
name = "node"
path = "dependencies/node/bin/node"

[resources.executables.npm]
name = "npm"
path = "dependencies/node/bin/npm"

[help]
prompt = "Use the managed Node.js runtime for JavaScript-based local tools. Prefer this path over system node when a plugin depends on node."
```

## Lifecycle

Plugins implement a common lifecycle:

```text
discover -> inspect -> install/repair -> activate -> export resources
```

- `discover`: load plugin manifests from the runtime bundle and user/plugin cache.
- `inspect`: read-only check of plugin status, versions, exported paths, and dependency state.
- `install/repair`: mutating setup flow. Downloads, extracts, verifies, or reinstalls managed dependencies.
- `activate`: registers capabilities and handlers with the daemon.
- `export resources`: contributes entries to the host resource registry.

`inspect` must never mutate the host. `getHostResource` must call only read-only paths or consume cached inspect results.

## Runtime Directory

The aHand managed runtime should live outside project workspaces:

```text
~/.cache/ahand-runtimes/ahand-primary-runtime/
  runtime.json
  plugins/
    shell/
    node/
    python/
    file/
    browser-playwright-cli/
  dependencies/
    node/
    python/
```

`runtime.json` records:

- bundle format version.
- bundle version.
- target platform.
- target architecture.
- installed plugin ids and versions.
- plugin status summary.
- managed Node/Python versions when present.

The runtime cache is owned by aHand. It should not contain user project dependencies.

## Host Resource Registry

The daemon should expose a read-only `getHostResource` API that returns installed plugin state and exported resources.

```ts
type HostResourceSnapshot = {
  runtimeVersion: string;
  platform: "darwin" | "linux" | "windows";
  arch: "arm64" | "x64";
  plugins: InstalledPluginResource[];
};

type InstalledPluginResource = {
  id: string;
  version: string;
  status: "installed" | "missing" | "outdated" | "failed" | "blocked";
  dependencies: string[];
  capabilities: string[];
  resources: Record<string, HostResourceValue>;
  helpPrompt?: string;
};

type HostResourceValue =
  | { kind: "executable"; name: string; path: string; version?: string }
  | { kind: "directory"; name: string; path: string }
  | { kind: "env"; name: string; value: string }
  | { kind: "config"; name: string; value: unknown };
```

Example:

```json
{
  "runtimeVersion": "0.1.0",
  "platform": "darwin",
  "arch": "arm64",
  "plugins": [
    {
      "id": "node",
      "version": "0.1.0",
      "status": "installed",
      "dependencies": [],
      "capabilities": [],
      "resources": {
        "node": {
          "kind": "executable",
          "name": "node",
          "path": "~/.cache/ahand-runtimes/ahand-primary-runtime/dependencies/node/bin/node",
          "version": "v24.14.0"
        },
        "npm": {
          "kind": "executable",
          "name": "npm",
          "path": "~/.cache/ahand-runtimes/ahand-primary-runtime/dependencies/node/bin/npm"
        }
      },
      "helpPrompt": "Use the managed Node.js runtime for JavaScript-based local tools. Prefer this path over system node when a plugin depends on node."
    },
    {
      "id": "browser-playwright-cli",
      "version": "0.1.0",
      "status": "installed",
      "dependencies": ["shell", "node"],
      "capabilities": ["browser"],
      "resources": {
        "playwrightCli": {
          "kind": "executable",
          "name": "playwright-cli",
          "path": "~/.cache/ahand-runtimes/ahand-primary-runtime/plugins/browser-playwright-cli/bin/playwright-cli"
        }
      },
      "helpPrompt": "Provides browser automation through playwright-cli. Use for browser open, click, fill, snapshot, screenshot, PDF, download, and close actions."
    }
  ]
}
```

Agents use this snapshot to decide which host capabilities are available and which executable paths to use. Missing or failed plugins should be visible, not silently hidden.

## Dependency Resolution

The plugin manager should resolve dependencies as a directed acyclic graph.

Rules:

- Missing dependency blocks activation of the dependent plugin.
- Failed dependency sets the dependent plugin to `failed` or `blocked`.
- Cycles are configuration errors and should fail daemon startup or plugin activation loudly.
- `install all` installs dependencies before dependents.
- `inspect all` reports every plugin independently and includes dependency status.

For the first plugin set:

```text
browser-playwright-cli
  -> shell
  -> node
```

`file`, `python`, `shell`, and `node` have no plugin dependencies.

## Capability Routing

The migration should avoid a risky protocol rewrite.

Stage 1 keeps existing protocol handlers:

- `JobRequest` continues to execute through current executor paths.
- `BrowserRequest` continues through current browser manager.
- `FileRequest` continues through current file manager.

But the setup and resource discovery path moves to plugins:

- `browser-doctor` calls plugin inspect.
- `browser-init` calls plugin install/repair.
- `BrowserManager` receives the `playwright-cli` path from `browser-playwright-cli` resources.

Stage 2 moves handler registration behind plugin activation:

- `shell` registers the `JobRequest` execution handler.
- `file` registers the file operation handler.
- `browser-playwright-cli` registers the browser operation handler.

The protocol can remain stable while dispatch becomes plugin-backed.

## Policy, Approval, and Audit

Plugins should not bypass existing safety controls.

Each capability plugin declares policy metadata:

- capability id.
- operation names.
- default risk level.
- approval escalation hints.
- config section ownership.

Existing checks remain authoritative:

- shell execution uses current session mode and command policy.
- file operations use current file policy.
- browser operations use current browser policy and domain controls.

The plugin registry only makes capability ownership explicit. It does not weaken policy enforcement.

## CLI Surface

Existing commands should keep working:

```bash
ahandd browser-doctor
ahandd browser-init
ahandd browser-init --step node
ahandd browser-init --step playwright
```

They should become compatibility wrappers over plugin commands:

```bash
ahandd plugin doctor
ahandd plugin doctor browser-playwright-cli
ahandd plugin install browser-playwright-cli
ahandd plugin repair node
```

The compatibility commands can be deprecated later, after dashboard and SDK callers use plugin APIs.

## Implementation Stages

### Stage 1: Runtime Plugin Foundation

- Add plugin manifest types.
- Add plugin registry and dependency graph.
- Add read-only inspect model.
- Add `getHostResource` snapshot model.
- Move Node setup logic behind the `node` plugin.
- Move playwright-cli setup logic behind the `browser-playwright-cli` plugin.
- Preserve current browser CLI commands as wrappers.

### Stage 2: Capability Plugin Activation

- Add capability registration hooks.
- Register shell execution through the `shell` plugin.
- Register file operations through the `file` plugin.
- Register browser operations through the `browser-playwright-cli` plugin.
- Keep existing wire protocol stable.

### Stage 3: Packaging and Bundle Updates

- Generate or ship first-party plugin packages.
- Write `runtime.json` during install/update.
- Add plugin cache repair flow.
- Add dashboard/admin plugin status UI.

## Testing Strategy

- Unit-test manifest parsing and validation.
- Unit-test dependency graph ordering and cycle rejection.
- Unit-test `getHostResource` serialization.
- Unit-test `node` and `browser-playwright-cli` inspect behavior with fake paths.
- Keep existing browser setup tests, migrating expectations to plugin APIs.
- Add integration tests that `browser-init` compatibility commands still work.
- Add tests that a missing `node` blocks `browser-playwright-cli` activation.

## First Implementation Decisions

- Plugin manifests should use TOML on disk because existing daemon configuration is TOML and the format is easy to inspect manually.
- Install state should be stored in `runtime.json` as the aggregate source of truth, with per-plugin status files added only if concurrent repair/update flows require them later.
- The first `getHostResource` surface should be JSON over the existing local/admin API boundary. A protobuf message can be added after the JSON shape stabilizes and hub forwarding needs it.
