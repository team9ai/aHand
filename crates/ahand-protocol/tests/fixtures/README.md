# Wire-format golden fixtures

Each `.bin` file in this directory is the prost-encoded byte output of one
`Envelope` message — one fixture per variant of `envelope::Payload`. The
fixtures are loaded by [`tests/golden_envelope.rs`](../golden_envelope.rs);
on every `cargo test` they are compared against freshly-encoded bytes, so
any drift in the wire format (renamed field, retagged variant, dropped
default, non-canonical encoding) shows up as a byte-level diff.

## When a test fails

Read the diagnostic message printed by the failing assertion. Two scenarios:

### 1. Drift was unintentional

Something accidentally changed the wire format — a field was renamed, a tag
was reused, a struct grew a default-non-zero field, etc. **Do not regenerate
the fixture.** Find and fix the protocol regression instead.

### 2. Drift was intentional (proto change is approved)

The protocol genuinely changed and every consumer (ahandd, ahand-hub, the
TypeScript SDK) is being updated in lockstep. Regenerate the fixtures with:

```bash
AHAND_FIXTURE_REGENERATE=1 cargo test -p ahand-protocol --test golden_envelope
```

This will overwrite every `.bin` file from the current Envelope shape. Review
the diff (`git diff -- crates/ahand-protocol/tests/fixtures/`) before
committing — the byte changes should match what you intended to change in
the proto.

## Adding a new payload variant

When `envelope.proto` gains a new `oneof payload` arm, the exhaustive match
in `golden_envelope.rs` (`payload_fixture_name`) **fails to compile** until
you:

1. Add a `golden_<variant>` test function with stable, hand-picked field
   values (mirror an existing one — pick values you'd recognise byte-by-byte
   in a hex dump).
2. Add a match arm to `payload_fixture_name` mapping the new variant to the
   fixture name, plus a `Default::default()` probe entry in
   `every_payload_variant_has_a_fixture_file`.
3. Generate the `.bin` file explicitly:
   ```bash
   AHAND_FIXTURE_REGENERATE=1 cargo test -p ahand-protocol --test golden_envelope
   ```
   The runner refuses to write fixtures unless `AHAND_FIXTURE_REGENERATE=1` is
   set — so a missing `.bin` from an accidental `git clean` won't silently
   re-seal whatever the current encoder produces.
4. Inspect `git diff -- crates/ahand-protocol/tests/fixtures/` and commit
   both `golden_envelope.rs` and the new `<variant>.bin`.

## Determinism caveats (read this if you edit the fixtures)

prost-encoded bytes are deterministic for scalars, repeated fields, and
single-entry maps, but **not** for `HashMap<K, V>` with two or more entries —
prost iterates the map in arbitrary order. Existing fixtures sidestep this
by giving `JobRequest.env` exactly one entry. Keep new fixtures the same way.

The decode→re-encode round trip in `assert_golden` will catch any future
regression on this front.
