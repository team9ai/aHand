#!/usr/bin/env node
import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

function parseArgs(argv) {
  const args = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) {
      throw new Error(`Unexpected argument: ${arg}`);
    }
    const key = arg.slice(2);
    const value = argv[index + 1];
    if (!value || value.startsWith("--")) {
      throw new Error(`Missing value for --${key}`);
    }
    args[key] = value;
    index += 1;
  }
  return args;
}

function requireString(record, key, file) {
  const value = record[key];
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${file}: expected non-empty string field ${key}`);
  }
  return value;
}

function resolveImageWithDigest(record, file) {
  if (
    typeof record.imageWithDigest === "string" &&
    record.imageWithDigest.length > 0
  ) {
    return record.imageWithDigest;
  }
  const image = requireString(record, "image", file);
  const digest = requireString(record, "digest", file);
  if (!digest.startsWith("sha256:")) {
    throw new Error(`${file}: digest must start with sha256:`);
  }
  return `${image.split("@sha256:", 1)[0]}@${digest}`;
}

function componentDirFor(record, file) {
  if (typeof record.componentDir === "string" && record.componentDir.length > 0) {
    return record.componentDir;
  }
  const deployment = requireString(record, "deployment", file);
  if (deployment === "ahand-hub") return "hub";
  if (deployment === "ahand-dashboard") return "dashboard";
  throw new Error(`${file}: unsupported aHand deployment ${deployment}`);
}

async function readMetadata(metadataDir) {
  const names = (await readdir(metadataDir))
    .filter((name) => name.endsWith(".json"))
    .sort();
  if (names.length === 0) {
    throw new Error(`No image metadata files found in ${metadataDir}`);
  }

  return Promise.all(
    names.map(async (name) => {
      const file = join(metadataDir, name);
      const record = JSON.parse(await readFile(file, "utf8"));
      return { file, record };
    }),
  );
}

export async function verifyGitopsManifests({ metadataDir, gitopsRoot }) {
  if (!metadataDir) {
    throw new Error("metadataDir is required");
  }
  if (!gitopsRoot) {
    throw new Error("gitopsRoot is required");
  }

  const metadata = await readMetadata(metadataDir);
  const checked = [];
  const errors = [];

  for (const { file, record } of metadata) {
    const namespace = requireString(record, "namespace", file);
    const deployment = requireString(record, "deployment", file);
    const componentDir = componentDirFor(record, file);
    const imageWithDigest = resolveImageWithDigest(record, file);
    const workloadPath = join(
      gitopsRoot,
      "apps/current/coffice-apps",
      namespace,
      "ahand-hub",
      componentDir,
      "workload.yaml",
    );

    let workload;
    try {
      workload = await readFile(workloadPath, "utf8");
    } catch (error) {
      errors.push(
        `${namespace}/${deployment}: cannot read ${workloadPath}: ${error.message}`,
      );
      continue;
    }

    if (!workload.includes(`image: ${imageWithDigest}`)) {
      errors.push(
        `${namespace}/${deployment}: missing image ${imageWithDigest}`,
      );
    }

    const env = record.env && typeof record.env === "object" ? record.env : {};
    for (const [key, value] of Object.entries(env)) {
      const stringValue = String(value);
      if (!workload.includes(`value: ${stringValue}`)) {
        errors.push(
          `${namespace}/${deployment}: missing env ${key}=${stringValue}`,
        );
      }
    }

    checked.push(`${namespace}/${deployment}`);
  }

  if (errors.length > 0) {
    throw new Error(errors.join("\n"));
  }

  return { checked };
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  const result = await verifyGitopsManifests({
    metadataDir: args["metadata-dir"],
    gitopsRoot: args["gitops-root"],
  });
  console.log(`Verified GitOps manifests: ${result.checked.join(", ")}`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
