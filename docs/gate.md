# Local merge gate

The only merge judge is `./scripts/merge-gate.sh` (aliases: `./scripts/como-el-ci.sh todo`, `./scripts/memory-industry-test.sh all`) exiting 0 **in this turn**, with a clean log: no `SKIPPED`, no soft alerts, no “should pass”.

GitHub Actions is not the merge judge. A green badge alone does not make a change mergeable.

Second judge, chained, not a substitute: `./scripts/quality-gate.sh` (lizard + `cargo mutants` on `rust/src` in the diff). See [AGENTS.md](../AGENTS.md).

## What it runs

| Step | What it proves |
|---|---|
| Postgres at `DATABASE_URL` (default `:5488`) | The cluster that URL names is up before anything mutates, and the gate prints the port that answered. Without `pg_isready` on `PATH`, the container that publishes the port of `DATABASE_URL` is asked, not one picked by name |
| `cargo audit` | Known Rust advisories, judged first: it refreshes the RustSec database itself (at most 120 s, under `timeout 120`); a database it cannot refresh is exit 2, never a skip |
| `cargo deny` | licenses, bans, sources; advisories are judged by `cargo audit` alone |
| Optional backup | Live corpus is snapshotted unless `SKIP_BACKUP=1` |
| `scripts/run-all-tests.sh` | fmt, clippy `-D warnings`, unit + ignored integration on throwaway `brain_gate` / peer, e2e MCP + live (no soft-skip), GPU placement vs `doctor --json`, eval smoke |
| `--features docs` | clippy + tests of the docs feature |
| npm wrapper smoke | `npm/install.test.js` |
| `codigo-muerto` | dead code / unused deps |
| `scripts/crap-gate.sh` | coverage floor (`CRAP_MIN_LINE_COV`, default 15%) |
| `scripts/mutants-gate.sh` | kill-rate floor on `src/search/{mmr,rrf,cache}.rs`; fails first if any unviable mutant was not refused by the compiler: its build must exit 1..255 and its log must show a rustc diagnostic in the build phase, not only cargo's `could not compile` or a linker failure. Unviable mutants leave the kill rate's denominator, so a machine out of memory would otherwise score high. A caught mutant counts only when its test run exited 1..255, or died of `0xC00000FD STATUS_STACK_OVERFLOW` (a mutant that recurses without end, accepted by name): a `cargo test` that never started is not a catch (`--self-test` runs the fixtures) |
| `scripts/quality-gate.sh` | **not** in the SIL — second judge: lizard + mutants of the `rust/src` diff. `--update-lizard-baseline` keeps every comment of `scripts/lizard-baseline.txt` over the key it explains, and names the ones that leave with their key |

`cargo audit` and `cargo deny` go before the tests because they cost seconds against ~20 minutes: a judge idle for days used to find a new advisory half an hour in. Measured, `cargo audit` takes 2.4 s with the database current, 11.6 s from a cold clone and 20.2 s to give up on a server that does not answer, so 120 s is a ceiling for a stalled transfer. It is never run with `--no-fetch` or `--stale`. Exit 124, or `couldn't fetch advisory database`, is exit 2 with a `FAIL:` that names `https://github.com/RustSec/advisory-db`; any other failure is exit 1, with cargo audit's own lines above it.

Oracles and fixtures decide green — not an AI opinion.

## What it does **not** check

Read this before trusting a green run (copied from the gate banner):

- **Reranker in E2E.** `e2e_all_tools.py` points `CUBA_RERANKER_PATH` at an empty dir so calls exercise the identity fallback. `mcp_live_session_test.py` inherits your shell and **does** use the real reranker. Both suites must stay.
- **GPU placement.** Only the *decision*, not the kernel. After the E2E, `scripts/gpu-placement-check.sh` reads `doctor --json` and fails when the `gpu` check disagrees with this machine: a fallback to CPU where an NVIDIA device **and** the ONNX Runtime provider libraries are both present, or a check that never names the CPU on a machine that has neither. That a kernel really executed on the GPU cannot be proven without a card — there is none in CI — and is still not covered here.
- **Retrieval quality.** Eval is a smoke run. It proves the harness executes; it asserts no nDCG threshold.
- **Other platforms.** The judge runs on the merge machine (typically Linux x64).
- **Migrations on old DBs.** The throwaway database is created from scratch. A migration that only works on an existing schema is still not covered.

## Fixtures

- Mutating tests write only to throwaway databases (`brain_gate` / peer). Never the live corpus.
- Eval smoke may read the live corpus **read-only**.
- Tests with no `#[ignore]` that need a server create and drop their own database, and never write into the one `DATABASE_URL` points at. A developer with `DATABASE_URL` exported towards their real memory would otherwise seed it by running `cargo test`.

## One gate at a time

Two gates on one machine share `brain_gate`, the ports and cargo's lock, and each one's exit used to drop the database the other was testing against. That happened: a gate left orphaned by a dead session fired its `trap` in the middle of the next run, and the failure read `database "brain_gate" does not exist` with nothing in it that pointed at the cause.

- `merge-gate.sh` takes the lock (`scripts/gate-lock.sh`) before anything else and holds it for the whole run. `run-all-tests.sh` inherits it only from its own live parent.
- A second gate refuses at once, before it sweeps, drops or builds anything, and names the first: pid, start time, command, and the `kill` that stops it.
- A lock whose owner died is taken over; one whose pid was reused is recognised by its start time. A lock from another host is refused, not guessed at.
- Every database the gate creates carries its run's record as a comment, and an exit drops only the ones still carrying its own.
- `~/.cache/cuba-gate/run.exit` (or `CUBA_GATE_EXIT_FILE`) is what to read when the session that launched a gate is gone. It belongs to the run that took the lock: `merge-gate.sh` for a whole gate, `run-all-tests.sh` only when run on its own. It says `running <owner record>` from the moment the lock is taken, and the exit code once the run ends. A gate killed in the middle runs no trap and leaves `running`, never an earlier run's `0`; the record's pid and start time say whether that run is still alive. Until this release it was written by `run-all-tests.sh` even under `merge-gate.sh`, so it could say `0` over a gate that then failed at the `--features docs` step, `codigo-muerto`, `crap-gate` or `mutants-gate`. The audit and deny steps now run before the tests, so a red there leaves its own exit code in the file and the tests never start. `./scripts/merge-gate.sh --self-test` drives a copy of the gate in a throwaway tree to prove each case.
- Published migrations through **0060** are frozen (SHA-384). Wrong shipped SQL gets a **new** migration. Do not edit `0017`–`0060`.

## Required machine

Missing any of these is **FAIL**, never `SKIPPED`:

- ONNX embed + ORT (`memory-industry models all`)
- NLI + reranker
- Generative LLM: `memory-industry llm set …` or `MEMORY_INDUSTRY_LLM_BASE_URL` or authenticated `claude`/`gemini` or MCP sampling
- `cargo-deny`, `cargo-machete`, `cargo-llvm-cov`, `cargo-audit`
- Network to `github.com`: the audit step fetches `https://github.com/RustSec/advisory-db` first. Without it the gate stops with exit 2 in seconds.
- `timeout` from GNU coreutils (`/usr/bin/timeout` in Git Bash and on Linux), not `C:\Windows\System32\timeout.exe`, which takes `/t` and turns the audit step red
- `node` on `PATH`, at least the major in `engines.node` of `package.json` (see below)

## Running the gate on Linux

The gate was written and measured on Windows under Git Bash. On Linux (WSL2, ext4) it runs as it is, with the Windows branches of the scripts (`cygpath … ||` fallbacks, `tr -d '\r'`, `.exe` candidates, the `WindowsApps` filter) left in place and still held by `rust/tests/local_gate_contract.rs`. A heavy run takes the machine's single heavy-work slot and its memory cap from `pesado`:

```bash
pesado -m 36G -w 7200 -n memory-industry-juez -- bash ./scripts/merge-gate.sh
```

Do not nest `pesado` inside `pesado`: the inner one waits on the lock the outer one holds. Everything that is launched by one `pesado` is under its cap, which is why the three points below are not optional.

**`gate_linux_env`** (`scripts/gate-lock.sh`) sets them up, and `merge-gate.sh`, `quality-gate.sh`, `mutants-gate.sh` and `release.sh` call it before their first step (`local_gate_contract.rs` holds each to it). On a machine that is not Linux it does nothing.

- **The cargo target is outside the tree.** `CARGO_TARGET_DIR` stays as you set it; unset, it becomes `${XDG_CACHE_HOME:-$HOME/.cache}/cargo-target/memory-industry`, absolute. `cargo mutants` copies the tree it mutates into a scratch directory, and with a target inside `rust/` (`rust/target-sil`) it copied 27.5 GB and the baseline of the mutants failed on binaries of two builds mixed. `--gitignore=false` stays in `quality-gate.sh`, but not because `eval-datasets` is git-ignored: it is versioned (`git ls-files rust/eval-datasets` lists its five `.jsonl`). Measured on 2026-10-13 with cargo-mutants 27.1.0, the copy reads `git_ignore: false` with the flag or without it, so the flag changes nothing there and dropping it would not keep a `target-*` out of the copy; see [Disk for the mutation step](#disk-for-the-mutation-step). The mutation scripts still `unset CARGO_TARGET_DIR` for the mutants themselves, so each scratch copy builds in its own `target/`.
- **`TMPDIR` is on the disk, not on tmpfs.** `/tmp` is tmpfs here and tmpfs is RAM: it counts against the `pesado` memory cap, and the scratch copy of every mutant would live in it. A `TMPDIR` that is already set and is not tmpfs is kept; otherwise it becomes `${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry-gate/tmp`, created. The filesystem is read with `stat -f -c %T`, so `tmpfs` and `ramfs` are the two it refuses.
- **`node` is required, not assumed.** The `npm wrapper smoke` step runs `node npm/install.test.js`, and the `node` on `PATH` is whichever nvm last selected. `gate_linux_env` reads the oldest major from `engines.node` in `package.json` (`>=18` is 18) and exits **2** when `node` is missing, older, or does not print a version, saying what is wrong and the fix (`nvm use 18`, or put a newer `node` first on `PATH`). It never goes on without it: the alternative is a red step forty minutes in that does not say why.

**`release.sh` with no receipt** runs `merge-gate.sh` as its child, so launch it inside `pesado` as well, with the same cap:

```bash
pesado -m 36G -w 7200 -n memory-industry-release -- bash ./scripts/release.sh v0.29.0
```

With a receipt for `HEAD` it runs nothing heavy and does not need it.

Every script that another script calls directly (`"$ROOT/scripts/mutants-gate.sh"`) has to be `100755` in the index: Windows creates `100644`, Git Bash does not mind, and Linux answers `Permission denied` at the first step. `every_gate_entry_point_is_executable_in_the_index` reads `git ls-files -s` and lists the ones that are not (`gate-lock.sh`, only ever sourced, is the one exception). `no_gate_script_points_into_a_windows_drive` keeps `/mnt/c/` and `/mnt/d/` out of `scripts/`.

## Running the gate on Windows

The gate was written on Windows under Git Bash and moved to WSL2 on 2026-10-03. Run natively again on 2026-10-12 (Windows 11, RTX 4070 Ti 12 GB), the core `run-all-tests.sh` took 20 min 35 s on 0.28.0 and found what a CPU-only Linux judge cannot see. This is what the machine has to be:

- **Git Bash, not WSL.** Launch every `.sh` with `C:\Program Files\Git\bin\bash.exe`; the `bash` on `PATH` is WSL. `cargo`, `cargo-mutants`, `cargo-deny`, `cargo-audit`, `cargo-machete`, `cargo-llvm-cov`, `lizard`, `psql` and a `node` of at least the major in `engines.node` have to be on `PATH`, as on Linux.
- **CUDA 12 and cuDNN 9, on `PATH`.** The ONNX Runtime CUDA provider (`onnxruntime_providers_cuda.dll`) depends on `cublasLt64_12.dll`, `cudart64_12.dll`, `cudnn64_9.dll` and their neighbours. A machine with only the CUDA 13 toolkit has none of them: the provider fails to register, the session runs on the CPU, `/health` says so (`gpu.degraded: true`, the reranker's `device: cpu` with the provider's cause) and `gpu-placement-check.sh` fails the gate, which is the gate telling the truth. The DLLs come from NVIDIA's `nvidia-cublas-cu12`, `nvidia-cuda-runtime-cu12`, `nvidia-cudnn-cu12`, `nvidia-cufft-cu12` and `nvidia-curand-cu12` wheels (the `.dll` files under `nvidia/*/bin/` in each); put them in one directory and that directory on `PATH` for the shell that launches the gate and for the daemon.
- **A Postgres of its own, on its own port, and `DATABASE_URL` pointing at it.** `pgvector/pgvector:pg18`, as `docker-compose.yml` has it. With `networkingMode=mirrored` in `.wslconfig`, Windows and WSL2 share `localhost`: the `:5488` that Windows reaches can be the Postgres of WSL, and the container of Docker Desktop then cannot publish that port at all. Ask the server which one answers: `psql "$DATABASE_URL" -Atc "select inet_server_addr()"` gives a `172.17.x.x` address for Docker Desktop and the WSL network's for WSL. Give the Windows container another port (`5489` was used) and export `DATABASE_URL=postgresql://cuba:…@127.0.0.1:5489/brain`; the gate reads host, port, user and database from it (`pg_isready -d "$DATABASE_URL"`) and prints the port that answered. A `DATABASE_URL` with a query (`?host=`, `?port=`) or without exactly one database segment is refused with exit 2, because `brain_gate` is derived from its tail: write a Unix socket as a percent-encoded host (`postgresql://cuba@%2Frun%2Fpostgresql/brain`) and pass options through `PG*` variables. `fsync=off`, `synchronous_commit=off` and `full_page_writes=off` are fine on this server and on no other: it is disposable.
- **`?sslmode=…` and every other query are refused, and the door that was open is closed.** `DATABASE_URL=…?sslmode=require` used to work, because `${url%/*}` does not cross the `?`; it is exit 2 now, with a reason that points at `PGSSLMODE` and never repeats the URL. Without `pg_isready` on `PATH`, the readiness check used to fall back to `docker exec memory-industry-db pg_isready`, a container judged by its name and not the server `DATABASE_URL` names. It now derives the port from `DATABASE_URL` as libpq does (`[::1]:5434`, else the last `:`, else `PGPORT`, else 5432) and asks `docker ps --filter publish=<port>`, which matches the host port whatever the container is called. Only a server of this machine (`127.0.0.1`, `localhost`, `::1`) can be judged that way. The verdicts: one container whose `pg_isready` answers is `OK  Postgres :<port> (container <name>)`; none publishing the port, or a Postgres that does not answer, is exit 1; more than one, a remote host or a socket, no `docker` on `PATH` or a `docker ps` that fails is exit 2, because nothing can say which server answers. Keep `pg_isready` on `PATH` anyway on a machine whose Postgres is not a local container.
- **One gate at a time, across both hosts.** The lock, the exit file and the receipts live in each host's own `~/.cache/cuba-gate`, so a gate in WSL and one in Windows do not see each other, and with mirrored networking they share `localhost`, the ports and, if they were given the same `DATABASE_URL`, `brain_gate`. Give them different servers, or never start both. A receipt written on Windows is a receipt for Windows: `release.sh` run from WSL does not find it.
- **No daemon needed.** A daemon holding the reranker on the card does not stop the gate (the card has room for two), but `target/release/memory-industry.exe` cannot be rebuilt while a process runs from it: run a daemon from a copy outside `target/`.

## Disk for the mutation step

`cargo mutants` copies `rust/` once per job and every copy compiles its own `target/`. On 2026-10-13 the step ran out of disk (`os error 112`) on this machine, and nothing had looked at the disk before copying. What was measured that day (Windows 11, Git Bash, cargo-mutants 27.1.0, a run of two mutants with `--jobs 2 --leak-dirs`):

- A copy weighed **27.58 GB**, of which **25.8 GB** were `target-*` directories of other builds that lived inside `rust/` (cargo-mutants leaves out only its own `rust/target`; a reflink was not possible, `reflink_used=false`).
- The build of each copy was another **14.1 GB**, 54 % of it `.pdb` files. With `CARGO_PROFILE_DEV_DEBUG=0` it is **8.94 GB**, 37 % less, and the baseline still passes.
- Linux is not measured. The figures above stand for both until it is.

`mutants-gate.sh` holds the rule in two modes that both judges call before `cargo mutants` (the main path of `mutants-gate.sh` inside the SIL, and `mutate_and_judge` in `quality-gate.sh`):

```text
jobs x GB per copy + reserve <= free GB on the scratch's volume
```

- `--disk-plan ROOT FREE_KB PER_COPY_KB RESERVE_KB WANT` is the pure rule. It prints `jobs=<n>` (the most that fit, at most `WANT`) and a `disk:` line; it adds `capped by disk: 3 wanted, 2 fit` when the disk cut them, and an explicit `MUTANTS_JOBS` is cut too. Not even one job fitting is **exit 1** with the numbers and `Nothing was copied or built`. A free space that could not be read is **exit 2**: not knowing is neither no room nor all the room there is.
- `--prepare-scratch WANT` creates the scratch directory, sweeps it, reads the free space with `df -Pk`, applies the rule and prints `jobs=` and `scratch=`.
- **The scratch is the judge's own directory**, `<temp>/memory-industry-mutants`, and `cargo mutants` gets it as `TMP`, `TEMP` and `TMPDIR`, for that command only. On Windows cargo-mutants reads `TMP`/`TEMP` and `TMPDIR` moves nothing; on Unix it reads `TMPDIR`. Everything the tests leave in it (`cuba-attrs-*`) is the judge's by construction.
- **`CARGO_PROFILE_DEV_DEBUG=0`** goes into the environment of that one `cargo mutants` command in both scripts, and is exported nowhere else.
- **A `target-*` inside `rust/` stops the step with exit 2**, naming the directory, before anything is copied: cargo writes a `CACHEDIR.TAG` in each, so it is seen instantly where `du` would take minutes. Delete it, or point `CARGO_TARGET_DIR` outside `rust/`.
- `check_builds` treats `os error 112`, `os error 28`, `There is not enough space on the disk` and `No space left on device` in a build's compile phase as **the machine stopping the build**, not as rustc refusing a mutant, and says to free disk and not memory.

| Variable | Default | What it does |
|---|---|---|
| `MUTANTS_SCRATCH_DIR` | `<temp>/memory-industry-mutants` | Another directory for the copies, the way out when `C:` is full and another volume has room. It has to be the judge's own: everything in it older than 6 h is swept, and it is refused if it is the system temp, `$HOME` or `/`. |
| `MUTANTS_SCRATCH_GB_PER_COPY` | 12 | Whole GB per copy. 12 is ceil(8.94 x 1.25), measured on Windows with debug info off. |
| `CUBA_GATE_MIN_FREE_GB` | 8 | The reserve that stays free; the same variable and value `run-all-tests.sh` uses. |
| `MUTANTS_JOBS` | from cores and RAM | Jobs wanted. The disk can lower it, never raise it. |

**What is swept**, before the disk is measured, so that what it frees counts:

- In the scratch, every entry whose own mtime is more than 6 h old.
- In the system temp, only leftovers from before the scratch existed, older than 6 h and provably the judge's: `cargo-mutants-rust-*.tmp` with a `Cargo.toml` that says `name = "memory-industry"`, `cuba-attrs-*`, and `tmp.*` holding a `mutants.out/`. A `cargo-mutants-rust-*.tmp` without that proof (a copy cut off halfway) is listed as `not provably the gate's, left alone`, with its size, and not touched.
- Never through a link, and a `rm` that fails (a file open on Windows) is reported as `could not remove` without failing the step: the measurement that follows decides.
- **Never while another gate is alive.** If `~/.cache/cuba-gate/lock` is held by a live process that is not this one's parent, it prints `another gate is running (...): sweep skipped` and carries on measuring. Under `merge-gate.sh`, which holds the lock and launches `mutants-gate.sh` as its direct child, it does sweep. `quality-gate.sh` takes no lock and is no different in that.
- `rust/target*` is not swept: those are the developer's, or `run-all-tests.sh`'s, which sweeps its own.

To measure the size per copy again, with no gate running and at least 60 GB free, from `rust/` in Git Bash:

```bash
unset CARGO_TARGET_DIR
SCR="$(cygpath -u "$TEMP")/g3-measure"; OUT="$SCR/out"; mkdir -p "$OUT"
TMP="$(cygpath -w "$SCR")" TEMP="$(cygpath -w "$SCR")" TMPDIR="$SCR" CARGO_PROFILE_DEV_DEBUG=0 \
  cargo mutants --file src/search/rrf.rs -F '<regex of two mutants of cargo mutants --list>' \
    --jobs 2 --timeout 90 --leak-dirs --output "$(cygpath -m "$OUT")" -- --lib
du -sk "$SCR"/cargo-mutants-rust-*.tmp      # KB per copy; GB x 1.25, rounded up, is the default
rm -rf "$SCR"
```

## Isolation extras (0.26)

`rust/tests/v041_isolation.rs` (`--ignored`) is the contract for:

- `cuba_jornada start` B while A is still open, under `cuba_app` (session is the control plane)
- `cuba_faro scope=errors` returns only errors
- `cuba_decreto query` on the exact title has `count >= 1`
- leftover `project_id NULL` rows stay hidden unless `include_unscoped=true`
- the same entity name can exist in two projects
- `Mcp-Session-Id` splits two chats that share `Mcp-Client-Id`
- quarantined observations do not surface in faro

E2E asserts `cuba_decreto` query `count >= 1` after record. That used to pass with count 0.

## Publishing

```bash
./scripts/release.sh v0.29.0               # gate (or its receipt), tag, push the tag
./scripts/release.sh --dry-run v0.29.0     # the same, print the tag message, create nothing
./scripts/release.sh --rerun-gate v0.29.0  # run the gate even over a receipt for HEAD
```

The only way to publish. In order, and any failure stops it before anything is created:

1. The tree is clean (untracked files count: the DB step discovers `rust/tests/*.rs` by glob), `HEAD` equals `origin/main` after a fetch, and the tag exists neither here nor on origin.
2. The tag's version is the one `rust/Cargo.toml` declares.
3. The gate's verdict on that `HEAD`, judged once:
   - If `~/.cache/cuba-gate/receipts/<HEAD sha>` holds a receipt for that commit (below), it is the verdict and the gate does **not** run again.
   - Otherwise, or with `--rerun-gate`, `./scripts/merge-gate.sh` runs on that `HEAD`, as it always did. It has to exit 0 **and** leave a clean log: no `SKIPPED` other than the banner's own "never SKIPPED", no `test result: FAILED`, a `MERGE GATE PASSED` line and a `kill_rate=` line. The log stays in `~/.cache/cuba-gate/release-<tag>.log`. The run says which of the three cases it is in.
4. An annotated tag is created whose message carries the receipt, one exact line `local-gate: MERGE GATE PASSED <40-char sha>`, plus the date and those two summary lines. Nothing from the environment goes in it.
5. Only that tag is pushed.

### The receipt

Publishing used to run the gate twice on one commit: green before the merge to `main`, then again inside `release.sh` on that same commit. A run takes over an hour on the merge machine, and on 2026-09-23 two runs back to back were both killed for memory. So `merge-gate.sh` now leaves its own verdict:

- It tees its whole output into `~/.cache/cuba-gate/merge-gate.log`, and on exit 0 judges that log with `gate_log_verdict` (`scripts/gate-lock.sh`), the same function `release.sh` judges its own log with.
- It writes `~/.cache/cuba-gate/receipts/<full sha>` only when that commit is all that was judged: `git status --porcelain` empty when the gate starts **and** when it ends, and `HEAD` where it started. The file is the four lines the tag carries under its title: `local-gate: MERGE GATE PASSED <sha>`, `local-gate-date:` (UTC, when the gate passed), and the `kill_rate=` and `MERGE GATE PASSED` summary lines.
- A dirty tree, a tree or `HEAD` that changed during the run, or an unclean log: no receipt, and the run says why in a `no receipt:` line. A dirty start is announced at the start too, not an hour later.
- A run that starts clean on a commit removes that commit's earlier receipt first: a red run on it takes the pass away, a green one writes it again.
- `release.sh` uses a receipt only if it is named after `HEAD` **and** its four lines are that shape for that sha. A copy of another commit's receipt under `HEAD`'s name runs the gate.

The receipt is a file in your home, not a signature: anyone who can write there can write one, as anyone who can push a tag could before. It saves a run; it does not add a judge.

`publish.yml` no longer asks GitHub whether `ci.yml` passed. Its first job fetches the annotated tag (`actions/checkout` leaves a lightweight ref in its place, [actions/checkout#290](https://github.com/actions/checkout/issues/290)) and runs `scripts/release.sh --verify-receipt <tag> <sha>`: the tag must be annotated, point at the commit being built, and carry the receipt line for that commit. Every other job needs it. A tag pushed by hand is refused, so the only way a release reaches npm, PyPI and the binaries is through a green local gate.

`ci.yml` runs only by `workflow_dispatch`. Its `release-matrix` job, which compiles the five published targets, no longer runs on its own: the local gate does not check other platforms, so dispatch it by hand before a release that touches features or platform code.

`./scripts/release.sh --self-test` drives each guard against a throwaway repo with a stand-in `merge-gate.sh`, and checks that the receipt it writes is the one `--verify-receipt` accepts. It also checks that a receipt for `HEAD` replaces the gate and its lines reach the tag, and that a receipt for another commit, one naming another commit inside, and `--rerun-gate` all run the gate. `./scripts/merge-gate.sh --self-test` checks the other half: a receipt after a green run over a clean tree, and none (with the reason) after an untracked file, a tree or `HEAD` that changed during the run, or an exit 0 over `SKIPPED`.

## Programming rules (this repo)

Versioned under `.cursor/rules/` (never a junction to `~/.cursor/rules`). Six-pack + TDD + two judges. Comments stay. Plant rules (Playwright, React Query, guardian-planta) are not copied here.

Handoffs: `.cursor/handoffs/*.yml`, validated by `scripts/validar-handoff.sh` — shape, plus the two-pass protocol: `commit` has to exist in the repo and `tests: written|frozen` (required when `commit != none`) is checked by extracting the `#[cfg(test)]` regions at that commit and in the tree. `written` fails when none moved, `frozen` when one did. An optional `paths:` list limits that comparison to the handoff's own slice, so that one agent's honest green pass is not refused for a test another agent edited in the same tree; it is refused empty, with a path that exists neither in the tree nor at `commit`, and with `commit: none`. `scripts/validar-handoff.sh --self-test` runs a fixture against each guard.
