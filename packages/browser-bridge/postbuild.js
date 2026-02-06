#!/usr/bin/env node
/**
 * Post-build script for browser-bridge.
 *
 * Fixes three issues with ncc's output:
 *  1. ncc names the output "index.js" — agent-browser expects "dist/daemon.js"
 *  2. ncc compiles into ESM but bundled code uses __dirname (CJS-only)
 *  3. ncc copies the source package.json (playwright-core's) into dist/
 *     which contains "type":"module" and other irrelevant fields
 *
 * This script:
 *  - Renames index.js → daemon.js
 *  - Prepends __dirname/__filename ESM polyfill
 *  - Writes a clean package.json with "type":"module"
 */
const fs = require("fs");
const path = require("path");

const distDir = path.join(__dirname, "dist");
const src = path.join(distDir, "index.js");
const dst = path.join(distDir, "daemon.js");
const pkg = path.join(distDir, "package.json");

if (!fs.existsSync(src)) {
  console.error("postbuild: dist/index.js not found — did ncc build succeed?");
  process.exit(1);
}

// 1. Inject __dirname polyfill for ESM compatibility.
const polyfill = [
  '// -- postbuild polyfill: provide __dirname/__filename in ESM context --',
  'import { fileURLToPath as __pb_ftu } from "url";',
  'import { dirname as __pb_dn } from "path";',
  "const __filename = __pb_ftu(import.meta.url);",
  "const __dirname = __pb_dn(__filename);",
  "// -- end polyfill --",
  "",
].join("\n");

let content = fs.readFileSync(src, "utf8");
fs.writeFileSync(dst, polyfill + content);
fs.unlinkSync(src);

// 2. Write a clean package.json (agent-browser daemon needs "type":"module"
//    for Node.js to load the ESM output; ncc copies playwright-core's
//    package.json which has extraneous fields).
fs.writeFileSync(
  pkg,
  JSON.stringify({ type: "module" }, null, 2) + "\n",
);

console.log("postbuild: dist/daemon.js ready (ESM polyfill injected, clean package.json)");
