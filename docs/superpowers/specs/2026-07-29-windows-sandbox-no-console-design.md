# Windows Sandbox Child Process Console Suppression

## Problem

Coffice 0.1.48 runs aHand's Windows restricted sandbox in-process. Each
restricted command launches a short-lived copy of `coffice-desktop.exe`, which
then starts the managed Python virtual environment through
`CreateProcessAsUserW`.

The current creation flags omit `CREATE_NO_WINDOW`. Windows therefore allocates
`conhost.exe` for the console-subsystem Python process. Repeated sandbox
commands appear to the user as high-frequency flashing black windows.

## Design

Add `CREATE_NO_WINDOW` to the flags passed to `CreateProcessAsUserW` in
`crates/ahandd/src/sandbox/platform/windows/process.rs`.

Keep the existing suspended-launch, explicit handle inheritance, job-object
assignment, and stdio capture flow unchanged. `CREATE_NO_WINDOW` suppresses
console allocation without changing the executable, arguments, environment,
security token, or captured standard streams.

## Testing

Extract the Windows process creation flags into a small function that returns
the production flag set. Add a Windows unit test asserting that the set
contains:

- `CREATE_NO_WINDOW`
- `CREATE_SUSPENDED`
- `CREATE_UNICODE_ENVIRONMENT`
- `EXTENDED_STARTUPINFO_PRESENT`

Run the focused Windows process tests, the aHand daemon test suite, formatting,
and Clippy. After the aHand change is pushed, update Coffice's pinned aHand Git
revision and run Coffice's Rust release-configuration tests and Cargo checks.

## Delivery

Commit and push the aHand fix first. Then pin Coffice to that exact aHand commit
so the change is reproducible in the next Windows release build.

No Python executable changes, sandbox policy changes, unrelated process-launch
refactors, or installer changes are included.
