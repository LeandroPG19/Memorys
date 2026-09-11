#!/usr/bin/env node
const { spawn, execFileSync } = require("child_process");
const path = require("path");
const fs = require("fs");
const { ensureBinary } = require("./postinstall.js");

const EXPECTED = require("../package.json").version;
const isWindows = process.platform === "win32";
const binName = isWindows ? "cuba-memorys.exe" : "cuba-memorys";
const localBin = path.join(__dirname, ".bin", binName);

function probeVersion(bin) {
  try {
    const out = execFileSync(bin, ["--version"], {
      timeout: 5000,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    });
    const m = out.match(/(\d+\.\d+\.\d+)/);
    return m ? m[1] : null;
  } catch {
    return null;
  }
}

async function resolveBinary() {
  if (fs.existsSync(localBin)) return localBin;

  try {
    return await ensureBinary();
  } catch (err) {
    process.stderr.write(
      `memory-industry: could not download the ${EXPECTED} binary — ${err.message}\n`
    );
  }

  const found = probeVersion(binName);
  if (found === EXPECTED) return binName;

  const reason =
    found === null
      ? "no memory-industry / cuba-memorys on PATH either (or it is too old to report its version)"
      : `PATH has cuba-memorys ${found}, but this package is ${EXPECTED}`;

  process.stderr.write(
    `memory-industry: cannot obtain the ${EXPECTED} binary — ${reason}.\n\n` +
      `The download on first run failed (offline, or GitHub Releases unreachable).\n` +
      `Get it another way:\n` +
      `  pip install memory-industry\n` +
      `  https://github.com/LeandroPG19/Memorys/releases/tag/v${EXPECTED}\n\n` +
      `Refusing to run a different version: this server migrates the database it\n` +
      `connects to, so the wrong binary does not just misbehave — it rewrites schema.\n`
  );
  process.exit(1);
}

resolveBinary().then((bin) => {
  const proc = spawn(bin, process.argv.slice(2), {
    stdio: "inherit",
    env: process.env,
  });
  proc.on("exit", (code) => process.exit(code ?? 1));
  proc.on("error", (err) => {
    process.stderr.write(`memory-industry: failed to start binary — ${err.message}\n`);
    process.exit(1);
  });
});
