// Fetch, verify and unpack the Argus release this package version belongs to.
//
// The package carries no binaries. They live in the GitHub release zip that
// install.ps1 also installs, so npm and the PowerShell installer put exactly
// the same files on disk, checked against the same SHA256SUMS.

"use strict";

const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");
const { spawnSync } = require("node:child_process");

const REPO = "hashimhashmi02/Argus";
const TARGET = "x86_64-pc-windows-msvc";
const VERSION = require("../package.json").version;
const ZIP_NAME = `argus-${VERSION}-${TARGET}.zip`;

/** Where the release is unpacked: Argus.exe, argus-engine.exe, argus-cli.exe, manifests\. */
const DIST = path.join(__dirname, "..", "dist");

function binary(name) {
  return path.join(DIST, name);
}

function installed() {
  return ["Argus.exe", "argus-engine.exe", "argus-cli.exe"].every((name) =>
    fs.existsSync(binary(name)),
  );
}

async function download(url) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`GET ${url} answered ${response.status} ${response.statusText}`);
  }
  return Buffer.from(await response.arrayBuffer());
}

/** The hash `sha256sum` format gives for `name`, or undefined. */
function expectedHash(sums, name) {
  for (const line of sums.split(/\r?\n/)) {
    const match = /^([0-9a-fA-F]{64})\s+\*?(.+)$/.exec(line.trim());
    if (match && match[2] === name) return match[1].toLowerCase();
  }
  return undefined;
}

/**
 * The zip and the SHA256SUMS it must match: from GitHub, or from a local
 * `ARGUS_ZIP` (a build from `scripts\package.ps1`, for testing or offline
 * installs), checked against the SHA256SUMS beside it.
 */
async function fetchRelease(log) {
  const local = process.env.ARGUS_ZIP;
  if (local) {
    log(`using ${local}`);
    const sums = path.join(path.dirname(local), "SHA256SUMS");
    return {
      zip: fs.readFileSync(local),
      name: path.basename(local),
      sums: fs.readFileSync(sums, "utf8"),
    };
  }
  const base = `https://github.com/${REPO}/releases/download/v${VERSION}`;
  log(`downloading Argus ${VERSION} from github.com/${REPO}`);
  const [zip, sums] = await Promise.all([
    download(`${base}/${ZIP_NAME}`),
    download(`${base}/SHA256SUMS`).then((b) => b.toString("utf8")),
  ]);
  return { zip, name: ZIP_NAME, sums };
}

/** Unpack with Windows' own Expand-Archive, so the package needs no dependencies. */
function unzip(zipPath, into) {
  const quote = (p) => `'${p.replace(/'/g, "''")}'`;
  const result = spawnSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-ExecutionPolicy",
      "Bypass",
      "-Command",
      `$ProgressPreference = 'SilentlyContinue'; Expand-Archive -LiteralPath ${quote(zipPath)} -DestinationPath ${quote(into)} -Force`,
    ],
    { stdio: ["ignore", "inherit", "inherit"], windowsHide: true },
  );
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`Expand-Archive failed (exit ${result.status})`);
}

async function install(log = (line) => console.error(`argus: ${line}`)) {
  if (process.platform !== "win32" || process.arch !== "x64") {
    throw new Error(`Argus runs on 64-bit Windows only, not ${process.platform} ${process.arch}`);
  }

  const { zip, name, sums } = await fetchRelease(log);
  const want = expectedHash(sums, name);
  if (!want) throw new Error(`SHA256SUMS has no entry for ${name}`);
  const got = crypto.createHash("sha256").update(zip).digest("hex");
  if (got !== want) {
    throw new Error(`${name} does not match its SHA256SUMS (expected ${want}, got ${got}); not installing it`);
  }

  // Unpacked beside the final folder and swapped in, so a failed unpack never
  // leaves half a release where a working one was.
  const staging = `${DIST}.new`;
  fs.rmSync(staging, { recursive: true, force: true });
  fs.mkdirSync(staging, { recursive: true });
  const zipPath = path.join(staging, name);
  fs.writeFileSync(zipPath, zip);
  unzip(zipPath, staging);
  fs.rmSync(zipPath);

  try {
    fs.rmSync(DIST, { recursive: true, force: true });
  } catch (error) {
    fs.rmSync(staging, { recursive: true, force: true });
    throw new Error(
      `could not replace ${DIST} (${error.code}). If the Argus engine is running from it, ` +
        "stop it with `argus-cli shutdown` and try again.",
    );
  }
  fs.renameSync(staging, DIST);
  log(`installed Argus ${VERSION}`);
}

/** Install now if `npm install --ignore-scripts` skipped it. */
async function ensureInstalled() {
  if (!installed()) await install();
}

module.exports = { binary, ensureInstalled, install, installed, expectedHash, VERSION };
