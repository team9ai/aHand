# Release Signing Keys

This directory contains the public key used to verify daemon auto-update binaries.

## Files

- `release.pub` — Raw 32-byte Ed25519 public key. Embedded in the hub at build time to verify
  update binary signatures before installation.

## How to generate a new keypair

```bash
# Generate a new Ed25519 private key (keep this SECRET, never commit it)
openssl genpkey -algorithm ed25519 -out release.pem

# Extract the raw 32-byte public key (DER header is 12 bytes, skip it)
openssl pkey -in release.pem -pubout -outform DER \
  | dd bs=1 skip=12 count=32 of=keys/release.pub

# Verify it is exactly 32 bytes
wc -c < keys/release.pub   # must print 32
```

Store `release.pem` in a secure secrets manager (e.g. 1Password, HashiCorp Vault).
**Never commit the private key to the repository.**

## How to sign a release binary

```bash
# Sign a binary using the private key
openssl pkeyutl -sign \
  -inkey release.pem \
  -rawin \
  -in ahandd-linux-aarch64 \
  -out ahandd-linux-aarch64.sig

# Verify the signature with the public key (for testing)
openssl pkeyutl -verify \
  -pubin -inkey <(openssl pkey -in release.pem -pubout) \
  -rawin \
  -in ahandd-linux-aarch64 \
  -sigfile ahandd-linux-aarch64.sig
```

The `.sig` file is a raw 64-byte Ed25519 signature. Upload both the binary and the `.sig`
file to the release download server, matching the URL templates configured in
`AHAND_HUB_UPDATE_DOWNLOAD_URL_TEMPLATE` and `AHAND_HUB_UPDATE_SIGNATURE_URL_TEMPLATE`.

## Key rotation

1. Generate a new keypair (see above).
2. Replace `keys/release.pub` in this repository with the new public key.
3. Re-sign all existing release binaries with the new private key, or keep the old binaries
   signed with the old key and ensure devices update before the old key is retired.
4. Deploy a new hub build (which will embed the new public key).
5. Retire the old private key from the secrets manager.

> Because the public key is embedded at build time, devices running an old hub version will
> continue using the old key. Coordinate key rotation with a hub deployment window.
