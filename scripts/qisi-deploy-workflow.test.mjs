import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";

const repoRoot = path.resolve(import.meta.dirname, "..");
const workflowPath = path.join(repoRoot, ".github", "workflows", "qisi-deploy.yml");

async function readWorkflow() {
  return (await readFile(workflowPath, "utf8")).replace(/\r\n/g, "\n");
}

function jobBlock(workflow, jobName) {
  const jobStart = workflow.indexOf(`  ${jobName}:\n`);
  assert.notEqual(jobStart, -1, `Missing workflow job: ${jobName}`);

  const nextJob = workflow.slice(jobStart + 1).search(/\n  [a-zA-Z0-9_-]+:\n/);
  return workflow.slice(
    jobStart,
    nextJob === -1 ? workflow.length : jobStart + 1 + nextJob,
  );
}

test("qisi deploy notifies Coffice to sync the embedded aHand dev dependency", async () => {
  const workflow = await readWorkflow();
  const job = jobBlock(workflow, "notify-coffice-ahand-dependency");

  assert.match(job, /needs: \[resolve-target, build-images, verify-gitops-manifests\]/);
  assert.match(job, /if: github\.ref_name == 'dev'/);
  assert.match(job, /COFFICE_REPO_DISPATCH_TOKEN/);
  assert.match(job, /event_type:"ahand-dev-updated"/);
  assert.match(job, /sha:env\.GITHUB_SHA/);
  assert.match(job, /gh api repos\/weightwave\/Coffice\/dispatches/);
});
