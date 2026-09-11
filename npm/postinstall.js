#!/usr/bin/env node
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

module.exports = { ensureBinary, binPath, artifactName, download };

if (require.main === module) {
  if (process.env.CUBA_MEMORYS_SKIP_INSTALL) {
    console.log("memory-industry: skipping binary install (CUBA_MEMORYS_SKIP_INSTALL set)");
    process.exit(0);
  }
  ensureBinary((m) => console.log(m)).catch((err) => {
    console.warn(`memory-industry: postinstall could not fetch the binary — ${err.message}`);
    console.warn("  It will be retried automatically the first time you run memory-industry.");
    process.exit(0);
  });
}
