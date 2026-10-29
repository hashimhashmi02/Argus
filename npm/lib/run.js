"use strict";

const { spawn } = require("node:child_process");
const { binary, ensureInstalled } = require("./release");

/**
 * Run one of the release's console programs here, in this terminal, and exit
 * with its exit code.
 *
 * Ctrl+C reaches the child directly, since it shares this console. Node
 * ignores it, so the child decides when to stop -- the engine saves its state
 * on the way out -- and this process waits for it.
 */
async function runConsole(name) {
  await ensureInstalled();
  process.on("SIGINT", () => {});
  const child = spawn(binary(name), process.argv.slice(2), { stdio: "inherit" });
  child.on("error", (error) => {
    console.error(`argus: could not start ${name}: ${error.message}`);
    process.exit(1);
  });
  child.on("exit", (code) => process.exit(code ?? 1));
}

/** Start the desktop app and return at once, leaving the window open. */
async function runWindow(name) {
  await ensureInstalled();
  const child = spawn(binary(name), process.argv.slice(2), {
    detached: true,
    stdio: "ignore",
  });
  child.on("error", (error) => {
    console.error(`argus: could not start ${name}: ${error.message}`);
    process.exit(1);
  });
  child.unref();
}

function fail(error) {
  console.error(`argus: ${error.message}`);
  process.exit(1);
}

module.exports = { runConsole, runWindow, fail };
