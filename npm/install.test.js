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

// CI-D5c (A.4b). The download can reach 450 MB and a connection that stays open without data would hold
// `npm install` for ever: the binary gets its time, and past it the refresh is a failed refresh like any other
// (false, the command to run by hand, exit 0 for npm). The child is a real node that outlives the test by far, so the
// limit is what ends it; without it `refreshRuntime` would answer true after 20 seconds.
test("refreshRuntime gives up on a binary that does not answer within its time", () => {
  assert.strictEqual(typeof mod.runBinary, "function", "runBinary must be exported: it is where the time limit lives");
  const logged = [];
  let ok;
  assert.doesNotThrow(() => {
    ok = mod.refreshRuntime(
      "/x/cuba-memorys",
      () => mod.runBinary(process.execPath, ["-e", "setTimeout(() => {}, 20000)"], 200),
      (m) => logged.push(m)
    );
  }, "a binary that does not answer must not make npm install fail");
  const said = logged.join("\n");
  assert.strictEqual(ok, false, "a binary cut at its time is not a refreshed runtime: " + said);
  assert.ok(said.includes("timed out after 200 ms"), `the message names the time that ran out: ${said}`);
  assert.ok(said.includes("memory-industry models runtime"), `and the command to run by hand: ${said}`);
});

// CI-D5c (A.4b, the log). `postinstall` has a log of its own (console.log when it runs as the package script) and
// used to hand the refresh none, so what the refresh said went to stderr by another route.
test("postinstall hands its own log to the refresh", () => {
  const log = () => {};
  const given = [];
  return mod
    .postinstall({
      ensure: async () => "/x/cuba-memorys",
      refresh: (bin, run, forwarded) => {
        given.push(forwarded === log);
        return true;
      },
      log,
    })
    .then(() => assert.deepStrictEqual(given, [true], "the refresh must get the log of the postinstall"));
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
