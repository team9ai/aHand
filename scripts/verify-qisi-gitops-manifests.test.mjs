import { mkdtemp, mkdir, writeFile } from "node:fs/promises";
import assert from "node:assert/strict";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { verifyGitopsManifests } from "./verify-qisi-gitops-manifests.mjs";

const image =
  "registry.image.cofficeai.cn/coffice/ahand/ahand-hub:dev-abc123";
const digest = "sha256:abcabcabc";
const imageWithDigest = `${image}@${digest}`;

async function writeFixture({
  workloadImage = imageWithDigest,
  sentryRelease = "abc123",
} = {}) {
  const root = await mkdtemp(join(tmpdir(), "ahand-qisi-gitops-"));
  const gitopsRoot = join(root, "gitops");
  const metadataDir = join(root, "metadata");
  const workloadDir = join(
    gitopsRoot,
    "apps/current/coffice-apps/coffice-dev/ahand-hub/hub",
  );

  await mkdir(workloadDir, { recursive: true });
  await mkdir(metadataDir, { recursive: true });

  await writeFile(
    join(metadataDir, "ahand-hub.json"),
    JSON.stringify(
      {
        namespace: "coffice-dev",
        deployment: "ahand-hub",
        container: "hub",
        componentDir: "hub",
        imageName: "ahand-hub",
        image,
        digest,
        imageWithDigest,
        env: {
          AHAND_HUB_IMAGE: imageWithDigest,
          GIT_SHA: "abc123",
          SENTRY_RELEASE: "abc123",
        },
      },
      null,
      2,
    ),
  );

  await writeFile(
    join(workloadDir, "workload.yaml"),
    `---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: ahand-hub
  namespace: coffice-dev
spec:
  template:
    spec:
      containers:
        - name: hub
          image: ${workloadImage}
          env:
            - name: AHAND_HUB_IMAGE
              value: ${workloadImage}
            - name: GIT_SHA
              value: abc123
            - name: SENTRY_RELEASE
              value: ${sentryRelease}
`,
  );

  return { gitopsRoot, metadataDir };
}

async function testAcceptsMatchingManifests() {
  const fixture = await writeFixture();

  const result = await verifyGitopsManifests(fixture);

  assert.deepEqual(result, {
    checked: ["coffice-dev/ahand-hub"],
  });
}

async function testRejectsStaleManifestDigest() {
  const fixture = await writeFixture({
    workloadImage: `${image}@sha256:old`,
  });

  await assert.rejects(
    () => verifyGitopsManifests(fixture),
    /missing image registry.image.cofficeai.cn/,
  );
}

await testAcceptsMatchingManifests();
await testRejectsStaleManifestDigest();
