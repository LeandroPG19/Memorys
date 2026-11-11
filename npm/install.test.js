#!/usr/bin/env node
const assert = require("assert");
const fs = require("fs");
const os = require("os");
const path = require("path");

let failures = 0;
function test(name, fn) {
  try {
    fn();
    console.log(`  ok   ${name}`);
  } catch (err) {
    failures++;
    console.error(`  FAIL ${name}\n       ${err.message}`);
  }
}

const mod = require("./postinstall.js");

test("postinstall exports the reusable download path", () => {
  assert.strictEqual(typeof mod.ensureBinary, "function", "ensureBinary must be exported");
  assert.strictEqual(typeof mod.binPath, "function", "binPath must be exported");
  assert.strictEqual(typeof mod.download, "function", "download must be exported");
});

test("bin.js loads and can require ensureBinary", () => {
  const src = fs.readFileSync(path.join(__dirname, "bin.js"), "utf8");
  assert.ok(src.includes('require("./postinstall.js")'), "bin.js must reuse postinstall");
  assert.ok(src.includes("ensureBinary"), "bin.js must call ensureBinary on first use");
});

test("binPath is platform-correct", () => {
  const p = mod.binPath();
  const expected = process.platform === "win32" ? "cuba-memorys.exe" : "cuba-memorys";
  assert.ok(p.endsWith(expected), `binPath ${p} should end with ${expected}`);
});

test("ensureBinary does NOT download when a real binary is already present", async () => {
  const dest = mod.binPath();
  const dir = path.dirname(dest);
  const hadDir = fs.existsSync(dir);
  const hadFile = fs.existsSync(dest);
  const backup = hadFile ? fs.readFileSync(dest) : null;
  try {
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(dest, "#!/bin/sh\necho present\n");
    let logged = "";
    const p = mod.ensureBinary((m) => (logged += m));
    return p.then((resolved) => {
      assert.strictEqual(resolved, dest, "should return the present binary");
      assert.ok(!logged.includes("downloading"), "must not download when present");
    });
  } finally {
    if (backup) fs.writeFileSync(dest, backup);
    else if (fs.existsSync(dest)) fs.unlinkSync(dest);
    if (!hadDir && fs.existsSync(dir)) fs.rmdirSync(dir);
  }
});

test("ensureBinary treats a 0-byte binary as missing", () => {
  const dest = mod.binPath();
  const dir = path.dirname(dest);
  const hadFile = fs.existsSync(dest);
  const backup = hadFile ? fs.readFileSync(dest) : null;
  try {
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(dest, "");
    assert.strictEqual(fs.statSync(dest).size, 0);
    assert.ok(fs.existsSync(dest) && fs.statSync(dest).size === 0);
  } finally {
    if (backup) fs.writeFileSync(dest, backup);
    else if (fs.existsSync(dest)) fs.unlinkSync(dest);
  }
});

// CI-D5b (P1-B). `npm install` of a new version asks the binary it just fetched to update the ONNX Runtime
// the machine already has, and only that one: `models runtime --if-installed` installs nothing on a machine
// that never had a runtime. The update can fail (no network, the daemon holds the DLL on Windows) and
// that must never fail `npm install`, which would leave the package without its binary.
test("postinstall exports the runtime refresh and the whole postinstall", () => {
  assert.strictEqual(typeof mod.refreshRuntime, "function", "refreshRuntime must be exported");
  assert.strictEqual(typeof mod.postinstall, "function", "postinstall must be exported");
});

test("refreshRuntime asks the binary to update a runtime that is already installed, and only that", () => {
  const calls = [];
  const logged = [];
  const ok = mod.refreshRuntime(
    "/x/cuba-memorys",
    (bin, args) => {
      calls.push([bin, args]);
      return 0;
    },
    (m) => logged.push(m)
  );
  assert.strictEqual(ok, true, "an exit code 0 is a refreshed runtime");
  assert.deepStrictEqual(
    calls,
    [["/x/cuba-memorys", ["models", "runtime", "--if-installed"]]],
    "one call, without --gpu: the flavor that is installed is the one that is kept"
  );
  assert.ok(!logged.some((m) => m.includes("could not")), "nothing to apologise for: " + logged.join(" | "));
});

test("refreshRuntime that fails says how to do it by hand and does not throw", () => {
  for (const [what, run] of [
    ["an exit code", () => 1],
    [
      "a binary that cannot be started",
      () => {
        throw new Error("spawn ENOENT");
      },
    ],
  ]) {
    const logged = [];
    let ok;
    assert.doesNotThrow(() => {
      ok = mod.refreshRuntime("/x/cuba-memorys", run, (m) => logged.push(m));
    }, `${what} must not make npm install fail`);
    assert.strictEqual(ok, false, `${what} is not a refreshed runtime`);
    const said = logged.join("\n");
    assert.ok(
      said.includes("memory-industry models runtime"),
      `${what}: the message names the command to run later: ${said}`
    );
  }
});

test("postinstall fetches the binary first and refreshes the runtime with it, and not when the fetch fails", () => {
  const order = [];
  const done = mod
    .postinstall({
      ensure: async () => {
        order.push("ensure");
        return "/x/cuba-memorys";
      },
      refresh: (bin) => {
        order.push(`refresh ${bin}`);
        return true;
      },
      log: () => {},
    })
    .then(() => {
      assert.deepStrictEqual(order, ["ensure", "refresh /x/cuba-memorys"]);
      const refreshed = [];
      return mod
        .postinstall({
          ensure: async () => {
            throw new Error("offline");
          },
          refresh: (bin) => refreshed.push(bin),
          log: () => {},
        })
        .then(
          () => assert.fail("a binary that could not be fetched is reported to the caller"),
          (err) => {
            assert.ok(err.message.includes("offline"));
            assert.deepStrictEqual(refreshed, [], "no binary, nothing to ask");
          }
        );
    });
  return done;
});

Promise.resolve().then(() => {
  setTimeout(() => {
    if (failures) {
      console.error(`\n${failures} test(s) failed`);
      process.exit(1);
    }
    console.log("\nall install smoke tests passed");
  }, 100);
});
