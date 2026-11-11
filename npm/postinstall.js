#!/usr/bin/env node
const { spawnSync } = require("child_process");
const https = require("https");
const fs = require("fs");
const path = require("path");

const VERSION = require("../package.json").version;
const REPO = "LeandroPG19/Memorys";
const BIN_DIR = path.join(__dirname, ".bin");

const PLATFORM_MAP = {
  "linux-x64": "cuba-memorys-linux-x64",
  "linux-arm64": "cuba-memorys-linux-arm64",
  "darwin-x64": "cuba-memorys-macos-x64",
  "darwin-arm64": "cuba-memorys-macos-arm64",
  "win32-x64": "cuba-memorys-windows-x64.exe",
};

function artifactName() {
  const key = `${process.platform}-${process.arch}`;
  const name = PLATFORM_MAP[key];
  if (!name) {
    throw new Error(
      `Unsupported platform: ${key}. Supported: ${Object.keys(PLATFORM_MAP).join(", ")}`
    );
  }
  return name;
}

function binPath() {
  const binName = process.platform === "win32" ? "cuba-memorys.exe" : "cuba-memorys";
  return path.join(BIN_DIR, binName);
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const file = fs.createWriteStream(dest);
    const fail = (err) => {
      file.close();
      fs.unlink(dest, () => {});
      reject(err);
    };
    const request = (u) => {
      https
        .get(u, (res) => {
          if (res.statusCode === 301 || res.statusCode === 302) {
            request(res.headers.location);
            return;
          }
          if (res.statusCode !== 200) {
            fail(new Error(`HTTP ${res.statusCode} for ${u}`));
            return;
          }
          const expected = Number(res.headers["content-length"]) || 0;
          let got = 0;
          res.on("data", (chunk) => {
            got += chunk.length;
          });
          res.pipe(file);
          file.on("finish", () => {
            file.close(() => {
              if (expected && got !== expected) {
                fs.unlink(dest, () => {});
                reject(new Error(`truncated download: got ${got} of ${expected} bytes`));
              } else {
                resolve();
              }
            });
          });
        })
        .on("error", fail);
    };
    request(url);
  });
}

async function ensureBinary(log = (m) => process.stderr.write(m + "\n")) {
  const dest = binPath();

  if (fs.existsSync(dest) && fs.statSync(dest).size > 0) {
    return dest;
  }

  if (!fs.existsSync(BIN_DIR)) {
    fs.mkdirSync(BIN_DIR, { recursive: true });
  }

  const name = artifactName();
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/${name}`;
  const tmp = dest + ".part";

  log(`memory-industry: downloading ${VERSION} binary from ${url}`);
  await download(url, tmp);
  fs.renameSync(tmp, dest);
  if (process.platform !== "win32") {
    fs.chmodSync(dest, 0o755);
  }
  log(`memory-industry: binary installed to ${dest}`);
  return dest;
}

// The download can reach 450 MB (the CUDA 12 package of Linux): 45 minutes hold it at ~200 kB/s, and past them a
// connection that stays open without data would hold `npm install` for ever. The binary has its own limits for a
// stalled peer; this one is for the binary itself.
const REFRESH_TIMEOUT_MS = 45 * 60 * 1000;

// The binary decides whether there is a runtime to update (`--if-installed`): the cache roots, HOME or
// USERPROFILE and the name of the library per system live in Rust, in one place. Its own output goes straight
// to the terminal. A binary that is cut (the time ran out, or a signal stopped it) throws, as one that cannot
// be started does, and `refreshRuntime` reads both as a refresh that did not happen.
function runBinary(bin, args, timeoutMs = REFRESH_TIMEOUT_MS) {
  const result = spawnSync(bin, args, { stdio: "inherit", timeout: timeoutMs });
  if (result.error && result.error.code === "ETIMEDOUT") {
    throw new Error(`timed out after ${timeoutMs} ms`);
  }
  if (result.error) throw result.error;
  if (result.signal) throw new Error(`stopped by ${result.signal}`);
  return result.status;
}

function refreshRuntime(bin, run = runBinary, log = (m) => process.stderr.write(m + "\n")) {
  const manual =
    "run `memory-industry models runtime` when you can (on Windows, with the daemon stopped)";
  let status;
  try {
    status = run(bin, ["models", "runtime", "--if-installed"]);
  } catch (err) {
    log(`memory-industry: could not update the ONNX Runtime — ${err.message}; ${manual}`);
    return false;
  }
  if (status !== 0) {
    log(`memory-industry: could not update the ONNX Runtime (exit ${status}); ${manual}`);
    return false;
  }
  return true;
}

// A failed refresh never fails the install: the package without its binary is worse than a runtime
// that is one version behind, which `doctor` reports and the command above fixes.
async function postinstall({ ensure = ensureBinary, refresh = refreshRuntime, log } = {}) {
  const bin = await ensure(log);
  refresh(bin, undefined, log);
  return bin;
}

module.exports = {
  ensureBinary,
  binPath,
  artifactName,
  download,
  runBinary,
  refreshRuntime,
  postinstall,
};

if (require.main === module) {
  if (process.env.CUBA_MEMORYS_SKIP_INSTALL) {
    console.log("memory-industry: skipping binary install (CUBA_MEMORYS_SKIP_INSTALL set)");
    process.exit(0);
  }
  postinstall({ log: (m) => console.log(m) }).catch((err) => {
    console.warn(`memory-industry: postinstall could not fetch the binary — ${err.message}`);
    console.warn("  It will be retried automatically the first time you run memory-industry.");
    process.exit(0);
  });
}
