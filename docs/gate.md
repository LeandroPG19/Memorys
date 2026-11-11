# Local merge gate

The only merge judge is `./scripts/merge-gate.sh` (aliases: `./scripts/como-el-ci.sh todo`, `./scripts/memory-industry-test.sh all`) exiting 0 **in this turn**, with a clean log: no `SKIPPED`, no soft alerts, no “should pass”. `./scripts/como-el-ci.sh rapido` is not a judge: four offline checks of `todo` (fmt, `codigo-muerto`, the npm smoke, the planner's table) in seconds, with no lock, no receipt and at low priority (`renice`, `ionice`). It is what the stop hook runs on every Stop. `./scripts/como-el-ci.sh parcial` is not one either: it runs rows P, B1, CT and DBG through the planner (`gate-dag.py run --rows`), takes the lock, writes `~/.cache/cuba-gate/parcial.exit` and `parcial.log`, leaves `run.exit` alone and writes no receipt: a first answer in minutes (CI-E29).

GitHub Actions is not the merge judge. A green badge alone does not make a change mergeable.

Second judge, chained, not a substitute: `./scripts/quality-gate.sh` (lizard + `cargo mutants` on `rust/src` in the diff). See [AGENTS.md](../AGENTS.md).

## What it runs

| Step | What it proves |
|---|---|
| Postgres at `DATABASE_URL` (no default: the judge's own server, marked `cluster_name=memory-industry-gate`, see [The judge's servers](#the-judges-servers)) | The cluster that URL names is up before anything mutates, and the gate prints the port that answered. Without `pg_isready` on `PATH`, the container that publishes the port of `DATABASE_URL` is asked, not one picked by name |
| `cargo audit` | Known Rust advisories, judged first: it refreshes the RustSec database itself (at most 120 s, under `timeout 120`); a database it cannot refresh is exit 2, never a skip; then the advisory exceptions (below) are judged against a report of the same database made without ignores |
| `scripts/advisory-exceptions.sh` | Every advisory the project ignores is in `scripts/advisory-exceptions.txt` with an owner, an expiry at most 180 days ahead and a reason, still matches an advisory of `rust/Cargo.lock`, and is ignored by `rust/.cargo/audit.toml` and `rust/deny.toml`, no more and no less. Red is exit 1, every problem at once; a report it cannot read is exit 2 |
| `cargo deny` | licenses, bans, sources; advisories are judged by `cargo audit` alone |
| `cargo fmt --check` | formatting, after `cargo deny` and before the first compile: a red here is seconds, not twenty minutes (CI-F1) |
| `codigo-muerto` | dead code / unused deps, before the first compile |
| npm wrapper smoke | `npm/install.test.js`, before the first compile |
| `run-all-tests.sh --check-models` | the embedding, NLI and reranker models are on disk under the paths the run exports, before the first compile |
| `scripts/run-all-tests.sh` | fmt, clippy `-D warnings`, unit + ignored integration on throwaway `brain_gate` / peer, e2e MCP + live (no soft-skip), GPU placement vs `doctor --json`, eval smoke on a throwaway `brain_gate_eval` seeded from `rust/eval-datasets/smoke-corpus.jsonl` |
| `--no-default-features` (row `DC`) | `clippy --all-targets -D warnings` of the build without the `docs` feature: compiled and linted, no test runs. `docs` is a default feature, so the rows before it run every test once with it (CI-K8) |
| `scripts/crap-gate.sh` | coverage floor (`CRAP_MIN_LINE_COV`, default 15%) |
| `scripts/mutants-gate.sh` | kill-rate floor on `src/search/{mmr,rrf,cache}.rs`; fails first if any unviable mutant was not refused by the compiler: its build must exit 1..255 and its log must show a rustc diagnostic in the build phase, not only cargo's `could not compile` or a linker failure. Unviable mutants leave the kill rate's denominator, so a machine out of memory would otherwise score high. A caught mutant counts only when its test run exited 1..255, or died of `0xC00000FD STATUS_STACK_OVERFLOW` (a mutant that recurses without end, accepted by name): a `cargo test` that never started is not a catch (`--self-test` runs the fixtures). CI-F4: each mutant builds only the lib test it runs (`--cargo-arg=--lib` before the `--`, `-- search::` after it), and the dependencies come from a seed keyed by content under `~/.cache/memory-industry-gate/mutants-seed/<clave>/target`, which no mutation run writes (each run mutates a stage made with `cp -a`). `--compare-classes REF NEW` judges two runs: exit 0 only if the names agree both ways, every class and every Test argv (argv[0] aside) is the same, both baselines are Success, and the totals add up. The seed is rebuilt if it is missing or half-built, keeps the two newest keys, and is removed if a run changed it. Tripwires named in the output: `OK  tree`, `OK  seed unchanged`, `OK  mutant set`, `seed: built`. A miss is time, not a red. F4d: `cargo-mutants` is called directly (`cargo-mutants mutants`, not `cargo mutants`) with `CARGO` pointing at `$OUT_DIR/bin/cargo-shim`. In a lab named `cargo-mutants-*.tmp` the shim gives every file of its `target/` the seed's newest mtime, once (a marker says it was done), because `fs::copy` gives each copy the time it was made, which cargo reads as stale and rebuilds (~204 crates); every other call goes to the real cargo with the same argv, stdout and exit. The epoch comes from `seed.before`, never from the start of the run. `lab builds: hit in k of k labs` (or `miss in m of k labs (<lab>: n crates in its first build)`) reads the first build of each lab in the order of `outcomes.json`, and `mtime shim: normalized a of k labs at @<epoch>` says how many labs got it; neither line is a verdict. The seed key records the profile the mutation builds force (`CARGO_PROFILE_DEV_DEBUG=0`, one source: `MUTANTS_PROFILE_ENV`), so an outside debug setting neither moves the key nor reaches the builds. `mutants-gate.sh --lab-builds OUTCOMES` prints the lab line for any `outcomes.json`. Known risk: a mutant that compiles in the lib and breaks another target would change class (none exists in `search/` today); the key includes the `rustc -vV`, so a seed does not carry across toolchains. CI-F6 (F6a): the call, the seed, the stage and the guards are `mutants_lab_run`, and `--lab-run OUT_DIR WANT_JOBS -- ARGS...` runs it for another script (`quality-gate.sh`): it plans the jobs from the disk (`--prepare-scratch`), prints `jobs=<n>`, runs cargo-mutants over ARGS, leaves its exit in `OUT_DIR/cargo-mutants.rc` and its output in `OUT_DIR/mutants.out`, and exits 0 if the guards held (whatever cargo-mutants said), 1 if one did not (the tree, the seed) and 2 for what the environment or the arguments lack. Both callers hand cargo-mutants `--jobserver-tasks <the CPUs the judge counts>`, see «The jobserver» below. CI-F6 (F6b): `--set-guard OUTCOMES LIST` is the guard of the mutant set for another script (exit 1 with `mutant set differs`, naming what is only on each side). The shim also reads `MUTANTS_SHIM_SELECT` (the table of tests that `quality-gate.sh` built), `MUTANTS_SHIM_ORIG` (the stage) and `MUTANTS_SHIM_SELECT_LOG`, which the lab sets when it is given a table: in a lab whose one marked file differs from the stage's at a line the table has, it adds `-- --exact <tests>` to the test phase of that call and nothing else, and every other call goes as it came with the reason (`full baseline`, `full markers-in-<n>-files`, `full no-entry <file>:<line>`) in the log, never on stdout; see [Test selection by coverage (Q2)](#test-selection-by-coverage-q2) |
| `scripts/test-times.sh` | Last step. The time of every test binary, summed over the gate's own log up to the npm step, against `scripts/test-time-ceilings.txt`: over its ceiling (or over the default 10 s with no line) is red, and so is under its floor, a line whose binary did not run, whose `rust/tests/<name>.rs` is gone, that expired or that is malformed. Prints the 10 slowest every run. A log it cannot read is exit 2 |
| `scripts/quality-gate.sh` | **not** in the SIL — second judge: lizard + mutants of the `rust/src` diff. `--update-lizard-baseline` keeps every comment of `scripts/lizard-baseline.txt` over the key it explains, and names the ones that leave with their key. CI-F6 (F6a): the mutation step has three modes, chosen by `QG_SELECT` (any other value is exit 2 before anything is mutated): `legacy`, the call of before F6 byte for byte (`cargo mutants … -- --lib`, a full copy of `rust/` per job), the reference of the study and the way back, and `full`, **the default**, the same mutants and the same verdict through the lab of `mutants-gate.sh` (`--lab-run`): each mutant builds only the lib test (`--cargo-arg=--lib`, nothing after a `--`) from the seed of `mutants-gate.sh`, which both judges share (same dependencies, same profile). The verdict of `full` is cargo-mutants' exit (0 green; 2 missed, 3 timeout, anything else red with `cargo-mutants rc=<n>`) plus `--check-builds` over its `outcomes.json` whenever there is one. The third, `cover`, runs each mutant with the tests that cover it and is **not the default**, see [Test selection by coverage (Q2)](#test-selection-by-coverage-q2). The output says `mode: legacy|full|cover`. `QG_KEEP_OUT=DIR` keeps the outcomes of the run in `DIR/<mode>/outcomes.json` (for `cover`, in `DIR/cover/`: the plan, the three passes and `merged.json`), green or red; a `DIR` that is not empty is exit 2 before anything is mutated, so two runs never mix. A mode that selects each mutant's tests by coverage is not the default until a study of the class of every mutant against `legacy` shows 0 differences, noted in the backlog. CI-F6 (F6c): `--study legacy\|full DIR` is that study, see [The equivalence study of Q2](#the-equivalence-study-of-q2---study) |

`cargo audit` and `cargo deny` go before the tests because they cost seconds against ~20 minutes: a judge idle for days used to find a new advisory half an hour in. Measured, `cargo audit` takes 2.4 s with the database current, 11.6 s from a cold clone and 20.2 s to give up on a server that does not answer, so 120 s is a ceiling for a stalled transfer. The judging run is never made with `--no-fetch` or `--stale`. The second call, for the exceptions, is: it reads the database the first one refreshed seconds earlier in the same run, and a second fetch would add a network failure and no information. Exit 124, or `couldn't fetch advisory database`, is exit 2 with a `FAIL:` that names `https://github.com/RustSec/advisory-db`; any other failure is exit 1, with cargo audit's own lines above it.

Oracles and fixtures decide green — not an AI opinion.

## Cargo jobs and the release build

- **Jobs.** `merge-gate.sh` counts the jobs of the machine once, before its first cargo call (`gate_cargo_jobs` in `scripts/gate-lock.sh`): `N = min(nproc, the CPUs of the cgroup, free RAM / 2 GiB)`, at least 1. The cgroup is the tightest of the chain this gate runs in (`cpu.max` up the chain, read by `gate_cgroup_read`), the free RAM is the lower of `MemAvailable` and what that chain leaves, and 2 GiB is what one rustc of this crate holds. No core is kept back: the judge holds the machine's single heavy-work slot, so a core it leaves idle is nobody's. It prints `cargo jobs=N (nproc=…, cgroup cpu=…, ram free=… GiB)` and exports `CARGO_BUILD_JOBS=N`; a row of `scripts/gate-dag.txt` that names its own `CARGO_BUILD_JOBS` gets the lower of that number and `N`. A measure that cannot be read (a cgroup file that is no number, no `MemAvailable`) is one job and a line that says what failed, never a refusal. A `CARGO_BUILD_JOBS` the operator sets is kept, and the line says so. `rust/.cargo/config.toml` keeps `jobs = 3` for the laptop, which the environment overrides on the judge.
- **Release.** One release compile, `cargo test --release --features docs,cuda --no-run --lib --test v017_rerank_gpu` (the set that `publish.yml` publishes on Linux x64, written out and held equal to it by a contract), takes `min(N, floor(margin GB / 4))` jobs (at least 1), with the margin the cgroup leaves. The gate does not call `scripts/build-gpu.sh`: that script caps its own memory and nests `systemd-run` and `nice` inside the scope that already limits the gate.
- **B3g.** `cargo build --release --features docs,cuda --bins --message-format=json` must report every unit `fresh`. A unit that compiled again means the binary the E2E would judge is not the one the `--no-run` built, and the gate stops by name.
- **sha256.** The binary's sha256 is taken after the `--no-run` build and checked after B3g, before the E2E and before the doctor. A change between those steps stops the gate with `sha256 changed`.

## What it does **not** check

Read this before trusting a green run (copied from the gate banner):

- **Reranker in E2E.** `e2e_all_tools.py` points `CUBA_RERANKER_PATH` at an empty dir so calls exercise the identity fallback. `mcp_live_session_test.py` inherits your shell and **does** use the real reranker. Both suites must stay.
- **GPU placement.** Only the *decision*, not the kernel. After the E2E, `scripts/gpu-placement-check.sh` reads `doctor --json` and fails when the `gpu` check disagrees with this machine: a fallback to CPU where an NVIDIA device **and** the ONNX Runtime provider libraries are both present, or a check that never names the CPU on a machine that has neither. That a kernel really executed on the GPU cannot be proven without a card — there is none in CI — and is still not covered here.
- **Retrieval quality.** Eval is a smoke run. It proves the harness executes; it asserts no nDCG threshold.
- **Other platforms.** The judge runs on the merge machine (typically Linux x64).
- **The build without docs.** `--no-default-features` is only compiled and linted (`clippy -D warnings`, row `DC`): no test runs without the `docs` feature, the default and what is published, with which every test runs once.

## Fixtures

- Mutating tests write only to throwaway databases (`brain_gate` / peer). Never the live corpus.
- The eval smoke never reads the live corpus. It scores a throwaway database seeded from a synthetic fixture.
- Tests with no `#[ignore]` that need a server create and drop their own database, and never write into the one `DATABASE_URL` points at. A developer with `DATABASE_URL` exported towards their real memory would otherwise seed it by running `cargo test`.

## The judge's servers

On Linux, with no `DATABASE_URL`, the judge makes its own Postgres for each row that names a `cluster:` in `scripts/gate-dag.txt`, a **lane**, and takes it down when the row ends: `b1`, `dbg`, `ct`, `d2`, `d3`, `nli`, `b3`, `rel` and `e2e` (the rows that call `run-all-tests.sh`), `cov` (`crap-gate.sh`) and `mut` (`mutants-gate.sh`); the first row (`P`) and the build without docs (row `DC`) open none, since they read no database. Each lane is a directory under `GATE_CLUSTER_ROOT` (by default `/tmp/memory-industry-gate-<uid>`, which is tmpfs), listens on its socket only (`listen_addresses` empty), has `fsync`, `synchronous_commit` and `full_page_writes` off, and is marked `cluster_name=memory-industry-gate-<lane>`. Its log goes to `${XDG_CACHE_HOME:-$HOME/.cache}/memory-industry-gate/clusters/<lane>.log` (or under `GATE_CLUSTER_LOGS`). A judge that died leaves its lanes behind: the next one sweeps the ones whose owner is gone, after taking the lock, and touches nothing else.

The versions are written once, in `scripts/gate-cluster.sh`, and this section says the same values:

- PostgreSQL 18.6 and pgvector 0.8.7, the upstream versions the judge's image ran (Debian 12, PGDG);
- locale `en_US.utf8` and time zone `Etc/UTC`, set at start and not in the data directory;
- packages from the PGDG repository (`resolute-pgdg`), signed by the key with fingerprint `B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8`, checked before it is trusted, and held with `apt-mark hold`.

Installed once, with `sudo` (dry run first; each block says what must come out):

    sudo install -d /etc/postgresql-common
    printf 'create_main_cluster = false\n' | sudo tee /etc/postgresql-common/createcluster.conf
    sudo install -d /usr/share/postgresql-common/pgdg
    sudo curl -fsS -o /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc
    gpg --show-keys --with-colons /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc | awk -F: '$1=="fpr"{print $10; exit}'   # B97B0AFCAA1A47F044F244A07FCC7D46ACCC4CF8, or delete the file and stop
    # the repository and its pin: /etc/apt/sources.list.d/pgdg.sources (Suites: resolute-pgdg, Signed-By the key) and /etc/apt/preferences.d/pgdg.pref (origin apt.postgresql.org, priority 100)
    sudo apt-get update
    sudo apt-get install postgresql-18=<18.6 revision of PGDG> postgresql-18-jit=<same> postgresql-18-pgvector=0.8.7-1.pgdg26.04+1
    sudo apt-mark hold postgresql-18 postgresql-18-jit postgresql-18-pgvector
    sudo locale-gen en_US.UTF-8
    /usr/lib/postgresql/18/bin/postgres --version      # PostgreSQL 18.6
    grep default_version /usr/share/postgresql/18/extension/vector.control   # '0.8.7'

Before each run the judge checks these and exits 2, in seconds and before anything else, if one is missing: the binaries `initdb`, `postgres`, `pg_isready`, `pg_config` and `psql`; the server version; `default_version` of pgvector; the locale; and that `GATE_CLUSTER_ROOT` is absolute, on tmpfs and yours. Nothing is started when it refuses.

To upgrade, change the constants in `scripts/gate-cluster.sh`, this section and the hold together, in one commit.

## An external server (Windows, or a server of your own)

With a `DATABASE_URL` the judge does not make lanes: every step runs against that server, and the gate says so with a `note:` line. The server must be marked as the judge's, as below.

The judge runs only against a Postgres whose `cluster_name` says it is the judge's. The mark decides, not the port and not the container's name: a server without it is refused, whatever it is called (the live corpus among them).

Start it with a container named by project, role and date, marked from its start command:

    docker run -d --name memory-industry-gate-20261106 --restart unless-stopped -p 127.0.0.1:5489:5432 -e POSTGRES_USER=<usuario> -e POSTGRES_PASSWORD=<clave> -e POSTGRES_DB=brain pgvector/pgvector:pg18 -c cluster_name=memory-industry-gate -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=300

Then `export DATABASE_URL='postgresql://<usuario>:<clave>@127.0.0.1:5489/brain'` and run `./scripts/merge-gate.sh`.

Check the mark with `psql` against the `postgres` database of that server: `show cluster_name` must print `memory-industry-gate`. The gate reads the same value read-only and prints `OK  judge server :<port> (cluster_name=…, system_identifier=…)` before `cargo audit`. Without `DATABASE_URL` the gate exits 2; a server that does not answer exits 1 with the `Postgres not ready` line; a server that answers without the mark exits 2.

## One gate at a time

Two gates on one machine share `brain_gate`, the ports and cargo's lock, and each one's exit used to drop the database the other was testing against. That happened: a gate left orphaned by a dead session fired its `trap` in the middle of the next run, and the failure read `database "brain_gate" does not exist` with nothing in it that pointed at the cause.

- `merge-gate.sh` takes the lock (`scripts/gate-lock.sh`) before anything else and holds it for the whole run. `run-all-tests.sh` inherits it only from its own live parent.
- A second gate refuses at once, before it sweeps, drops or builds anything, and names the first: pid, start time, command, and the `kill` that stops it.
- A lock whose owner died is taken over; one whose pid was reused is recognised by its start time. A lock from another host is refused, not guessed at.
- Every database the gate creates carries its run's record as a comment, and an exit drops only the ones still carrying its own.
- `~/.cache/cuba-gate/run.exit` (or `CUBA_GATE_EXIT_FILE`) is what to read when the session that launched a gate is gone. It belongs to the run that took the lock: `merge-gate.sh` for a whole gate, `run-all-tests.sh` only when run on its own. It says `running <owner record>` from the moment the lock is taken, and the exit code once the run ends. A gate killed in the middle (SIGKILL: the kernel under memory pressure) runs no trap and leaves `running`, never an earlier run's `0`; one stopped by a signal it can catch (SIGTERM from a timeout or a service manager, SIGHUP from a closed terminal) runs the trap, which used to write the status of the last command, a `0` over a run that had finished nothing (found on 2026-10-20), and now writes `2`, the status for «could not judge», says so, and leaves no receipt: only the last line of the script marks the gate as having reached its end. The process itself ends with the signal's status (143, 129), because bash raises it again after the trap, so the verdict is the file. The gate does not forward the signal to the `run-all-tests.sh` it launched, which keeps running as an orphan until it ends; stop it by hand; the record's pid and start time say whether that run is still alive. Until this release it was written by `run-all-tests.sh` even under `merge-gate.sh`, so it could say `0` over a gate that then failed at the step of the build without docs, `codigo-muerto`, `crap-gate` or `mutants-gate`. The audit and deny steps now run before the tests, so a red there leaves its own exit code in the file and the tests never start. `./scripts/merge-gate.sh --self-test` drives a copy of the gate in a throwaway tree to prove each case.
- Published migrations through **0060** are frozen (SHA-384). Wrong shipped SQL gets a **new** migration. Do not edit `0017`–`0060`.

## Required machine

Missing any of these is **FAIL**, never `SKIPPED`:

- ONNX embed + ORT (`memory-industry models all`)
- NLI + reranker
- Generative LLM: `memory-industry llm set …` or `MEMORY_INDUSTRY_LLM_BASE_URL` or authenticated `claude`/`gemini` or MCP sampling
- `cargo-deny`, `cargo-machete`, `cargo-llvm-cov`, `cargo-audit`
- `rustup`: `rust-toolchain.toml` at the root pins the compiler (an exact `N.N.N`, never `stable`) with the components and the release targets the gate and `publish.yml` use. Raising the version is one commit that changes `channel`; `rust/tests/local_gate_contract.rs` holds the file to what the scripts and workflows name. Run `rustup toolchain install` from the root once so it installs them.
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
- **A Postgres of its own, on its own port, and `DATABASE_URL` pointing at it.** `pgvector/pgvector:pg18`, as `docker-compose.yml` has it. With `networkingMode=mirrored` in `.wslconfig`, Windows and WSL2 share `localhost`: the port of the live corpus that Windows reaches can be the Postgres of WSL, and the container of Docker Desktop then cannot publish that port at all. Ask the server which one answers: `psql "$DATABASE_URL" -Atc "select inet_server_addr()"` gives a `172.17.x.x` address for Docker Desktop and the WSL network's for WSL. Give the Windows container another port (`5489` was used) and export `DATABASE_URL=postgresql://cuba:…@127.0.0.1:5489/brain`; the gate reads host, port, user and database from it (`pg_isready -d "$DATABASE_URL"`) and prints the port that answered. A `DATABASE_URL` with a query (`?host=`, `?port=`) or without exactly one database segment is refused with exit 2, because `brain_gate` is derived from its tail: write a Unix socket as a percent-encoded host (`postgresql://cuba@%2Frun%2Fpostgresql/brain`) and pass options through `PG*` variables. `fsync=off`, `synchronous_commit=off` and `full_page_writes=off` are fine on this server and on no other: it is disposable.
- **`?sslmode=…` and every other query are refused, and the door that was open is closed.** `DATABASE_URL=…?sslmode=require` used to work, because `${url%/*}` does not cross the `?`; it is exit 2 now, with a reason that points at `PGSSLMODE` and never repeats the URL. Without `pg_isready` on `PATH`, the readiness check used to fall back to `docker exec memory-industry-db pg_isready`, a container judged by its name and not the server `DATABASE_URL` names. It now derives the port from `DATABASE_URL` as libpq does (`[::1]:5434`, else the last `:`, else `PGPORT`, else 5432) and asks `docker ps --filter publish=<port>`, which matches the host port whatever the container is called. Only a server of this machine (`127.0.0.1`, `localhost`, `::1`) can be judged that way. The verdicts: one container whose `pg_isready` answers is `OK  Postgres :<port> (container <name>)`; none publishing the port, or a Postgres that does not answer, is exit 1; more than one, a remote host or a socket, no `docker` on `PATH` or a `docker ps` that fails is exit 2, because nothing can say which server answers. Keep `pg_isready` on `PATH` anyway on a machine whose Postgres is not a local container.
- **One gate at a time, across both hosts.** The lock, the exit file and the receipts live in each host's own `~/.cache/cuba-gate`, so a gate in WSL and one in Windows do not see each other, and with mirrored networking they share `localhost`, the ports and, if they were given the same `DATABASE_URL`, `brain_gate`. Give them different servers, or never start both. A receipt written on Windows is a receipt for Windows: `release.sh` run from WSL does not find it.
- **No daemon needed.** A daemon holding the reranker on the card does not stop the gate (the card has room for two), but `target/release/memory-industry.exe` cannot be rebuilt while a process runs from it: run a daemon from a copy outside `target/`.
- **When it runs: once per release, before `release.sh`, not on every change** (Leandro, 2026-11-05). The 15-minute judge of the days between is the Linux one; the Windows run is what covers the GPU, which the CPU-only Linux judge cannot, and it sits outside that cycle. Its receipt is a receipt for Windows (see «One gate at a time, across both hosts» above). Since the planner (F3.3b-2) the judge on Windows needs a `python3` that imports `scripts/gate-dag.py`: `gate_history.py` does `import fcntl` and `gate-dag.py` reads `signal.SIGHUP` at module level, and neither exists in a native Windows Python (CI-E25b, found by reading, not yet run on Windows). Check it first, in Git Bash: `python3 -I -B scripts/gate-dag.py rows --table scripts/gate-dag.txt`.

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
- **The scratch is the judge's own directory**, `<dir>/memory-industry-mutants`, where `<dir>` is `MUTANTS_SCRATCH_DIR` or the system temp. It is never `<dir>` itself: `MUTANTS_SCRATCH_DIR=D:\` makes `D:\memory-industry-mutants`, and the rest of `D:\` is not the sweep's business. `cargo mutants` gets the scratch as `TMP`, `TEMP` and `TMPDIR`, for that command only. On Windows cargo-mutants reads `TMP`/`TEMP` and `TMPDIR` moves nothing; on Unix it reads `TMPDIR`. Everything the tests leave in it (`cuba-attrs-*`) is the judge's by construction.
- **The marker `.memory-industry-mutants`** inside the scratch is what proves the directory is the judge's. The script writes it only when the directory does not exist or is empty, and the sweep leaves it out. A `memory-industry-mutants` that holds something and has no marker is not swept: it prints `<dir> is not provably the gate's scratch, left alone`, and the marker is not written into it either, so it is never adopted. Delete or empty it by hand to let the judge take it. The scratch is refused with exit 2, before anything is swept, if it holds the repo or `$HOME`.
- **`CARGO_PROFILE_DEV_DEBUG=0`** goes into the environment of that one `cargo mutants` command in both scripts, and is exported nowhere else.
- **A `target-*` inside `rust/` stops the step with exit 2**, naming the directory, before anything is copied: cargo writes a `CACHEDIR.TAG` in each, so it is seen instantly where `du` would take minutes. Delete it, or point `CARGO_TARGET_DIR` outside `rust/`.
- **`merge-gate.sh` asks the same at the start**, right after setting up its environment and before the Postgres check and `=== cargo audit ===`, through `mutants-gate.sh --refuse-targets`: it used to be refused only in the last step, ~20 min of tests later. The mode also refuses, exit 2, a `CARGO_TARGET_DIR` that points inside `rust/` other than `rust/target` (a relative value counts from `rust/`), whether or not the directory exists yet, because cargo creates it at the first build. Nothing is created and nothing is printed when it passes.
- **`lizard` is required** by `quality-gate.sh` for the complexity half: without it on `PATH` (`python` alone is no longer enough) it exits 2 with `lizard is not on PATH`, and a `lizard` that exits non-zero is exit 2 too. An empty list of functions over the ceiling would read as green.
- `check_builds` treats `os error 112`, `os error 28`, `There is not enough space on the disk` and `No space left on device` in a build's compile phase as **the machine stopping the build**, not as rustc refusing a mutant, and says to free disk and not memory.

| Variable | Default | What it does |
|---|---|---|
| `MUTANTS_SCRATCH_DIR` | the system temp | The directory in which `memory-industry-mutants` is made, the way out when `C:` is full and another volume has room. Only that subdirectory is swept, and only with its marker; the scratch is refused if it is the system temp, `$HOME` or `/`, or if it holds the repo or `$HOME`. |
| `MUTANTS_SCRATCH_GB_PER_COPY` | 12 | Whole GB per copy. 12 is ceil(8.94 x 1.25), measured on Windows with debug info off. |
| `CUBA_GATE_MIN_FREE_GB` | 8 | The reserve that stays free; the same variable and value `run-all-tests.sh` uses. |
| `MUTANTS_JOBS` | from the cores and the memory that is available ([below](#memory-for-the-mutation-step)) | Jobs wanted. A number named here is kept as named and the memory is not read. The disk can lower it, never raise it. |

**What is swept**, before the disk is measured, so that what it frees counts:

- In the scratch, with its marker, every entry whose own mtime is more than 6 h old except the marker. Beside it, in `MUTANTS_SCRATCH_DIR`, nothing.
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

## Memory for the mutation step

On 2026-10-14 the WSL VM had 63 GB in total and about 10 GB free. The jobs came out of the total (`min(cores/2, total GB/4, 6)` = 6), two builds died in rustc's `handle_alloc_error`, and the judge, rightly, refused them (`check_builds`): 48 minutes for a choice of jobs that was wrong, not a mutant that was. The rule now reads the memory that is **available**, and it lives once.

```text
jobs = min(cores / 2, GB available / 4, 6), never below 1
```

- `mutants-gate.sh --available-kb [MEMINFO]` prints the KB the machine can still give: `MemAvailable` of `/proc/meminfo`, else `MemFree` (Git Bash's `/proc/meminfo` has no `MemAvailable`; `MemFree` undercounts what is cached and never overcounts it), else the OS is asked (`FreePhysicalMemory`, for a Windows shell with no `/proc`). **The total is not read.** Not knowing is **exit 2**, with the way out in the message: `MUTANTS_JOBS=<n>`.
- `--jobs-plan CORES AVAILABLE_KB` is the pure rule. It prints `jobs=<n>` and a `memory:` line, adds `capped by memory: 12 wanted by cores, 2 fit` when the memory lowered the number, and says `below the 4 GB one job is measured to need` when there is less than one job's worth (it still runs 1: a build the machine kills for want of memory is `--check-builds`' to say, out loud, and not this rule's to refuse). A value that is not a whole number, or 0 cores, is exit 2.
- `--default-jobs [CORES MEMINFO]` is both, read from this machine when given nothing. `quality-gate.sh` asks it; it keeps no formula of its own (the comment over the old copy said «same derivation as mutants-gate.sh», and that is the line that drifted). `local_gate_contract.rs` fails if either script reads `MemTotal` outside its `--self-test`, or if `quality-gate.sh` stops asking.
- 4 GB per job is the calibration the formula started from (a 14.9 GB laptop ran 3); 6 is the ceiling because each run carries a 90 s test timeout and a saturated machine turns a caught mutant into a reported TIMEOUT.
- **A named `MUTANTS_JOBS` is not cut by the memory** (the disk does cut it): the operator who writes the number answers for it, and a build the machine then kills is still refused by `--check-builds` with the cause. `build-gpu.sh` still sizes its release build from `MemTotal`; it is not this rule.

```bash
./scripts/mutants-gate.sh --jobs-plan 24 10485760     # the incident: jobs=2
./scripts/mutants-gate.sh --default-jobs              # this machine
```

## The jobserver of the mutation step (CI-F6)

cargo-mutants 27.1.0 turns its jobserver on by default, with `--jobserver-tasks` tokens (the number of CPUs it sees when it is not told) and hands it to every process it starts, builds and tests. A cargo that inherits a jobserver takes its parallelism from the tokens and **ignores `-j` and `CARGO_BUILD_JOBS`** (read in cargo's source, `BuildRunner::new` and its `jobserver_and_j` test; not measured here). So the `CARGO_BUILD_JOBS=⌊N/J⌋` that CI-F1 (D-F1b) gives each mutant build bounded nothing under cargo-mutants, and the `rustc`s of the J labs could add up to more than the CPUs the judge counts. Both judges now pass `--jobserver-tasks <GATE_CARGO_JOBS>`, the count `gate_cargo_jobs` makes from `nproc` and the cgroup of `pesado` (`cpu.max`). Whether `num_cpus` would have read that cgroup was **not verified**; passing the number removes the doubt. The `CARGO_BUILD_JOBS` per build stays, harmless; removing it waits for a measure (F5).

## Test selection by coverage (Q2)

`QG_SELECT=cover` judges the same mutants with fewer tests, and says so in the output (`mode: cover`). It is not the default and is not adopted until a study of the class of every mutant against `legacy` shows 0 differences, noted in the backlog (D-F6q): what it spares is time, and it earns that only if no class changes.

**The rule.** A map says which functions each lib test enters: `cargo llvm-cov show-env --sh --no-cfg-coverage` instruments the lib test into `<the judge's target>/qg-cover` (a directory of its own: `crap-gate.sh` builds with `cfg(coverage)` into the same target), and every test that is not ignored runs alone, `P` at a time (`P` is the CPUs the judge counts), with a profile of its own. `llvm-cov export` gives the functions it entered (`count` above 0, in `rust/src`), reduced to the first and last line of their regions in that file; the profile, the merged profile and the export are deleted as soon as they are reduced. A test covers a mutant if a function it entered is in the mutant's file and its range holds the line the mutant starts on. That is a superset of the tests that run the line, so a test that could catch the mutant is never left out. `cover map: <n> tests in <s> s (build <b> s), <f> functions entered` is the line of the map. Not measured yet: the map has only run against stand-ins of the llvm tools; the first run with the real ones is the smoke test of the last run of the batch.

**Three passes, in this order.** A0: the mutants no test covers (`no-coverage`), and those whose subset failed without a mutant (`preflight`), with the whole suite. A1: the rest, each with its subset: one invocation of cargo-mutants, with a `--re '^<name, escaped as regex::escape does>$'` per mutant, and the table of tests in `MUTANTS_SHIM_SELECT`, so that the shim of `mutants-gate.sh` adds `-- --exact <tests>` to the test phase of each mutant's lab. B: every mutant that A1 left missed or timed out, with the whole suite. Each pass has to measure exactly the mutants it was asked for (`set differs in pass <X>`), and goes through the build check as it finishes.

**Q3.** When A0 ends, before A1 starts, the step prints `early: <u> mutants with no covering lib test ran first: <c> caught, <m> missed, <t> timeout` and the names of the missed ones. It does not stop there: the green still needs every mutant, and the reds are all in one run.

**Why a `caught` with a subset is a `caught`.** The test that fails with the mutant is in the subset, and it is also in the whole suite, so the whole suite would have failed too. The converse is not true, and that is what B is for: a mutant that its subset does not catch (missed, or timed out) is run again with the whole suite, and its class is B's. Every missed and every timeout is confirmed with the whole suite before it is a verdict: `merged.json` (the mode `quality-gate.sh --merge-select OUT A0 A1 B`) takes A0's class for A0, A1's for a caught or unviable, B's for the rest, and is red if a confirmation is missing, a baseline is not Success or the totals do not add up. The verdict is the merged one: a missed or timed out mutant, named, and nothing else; the exit 2 of cargo-mutants in A1 is not a verdict by itself. The subset's premise is that it passes without the mutant, so each distinct subset runs once on the instrumented binary before any mutant does; one that fails sends its mutants to A0 and is named (`preflight`).

**When `cover` does not run.** Without `cargo-llvm-cov`, or without `llvm-profdata` and `llvm-cov` in the sysroot of `rustc` (`rustup component add llvm-tools`), it is exit 2 naming what is missing, before anything is mutated. A test that fails alone in the map (or does not finish in `QG_COVER_TEST_TIMEOUT_S`, 300 by default), a coverage that cannot be read, a lib test that does not build instrumented, a `--list` with no mutants, or cargo-mutants' marker in `rust/src` (the shim would take a lab for a mutated one) turn it off: `cover: off (<why>)`, `mode: full`, and the step gives the verdict `full` gives. That is time lost and never a class changed.

## The equivalence study of Q2 (`--study`)

`bash scripts/quality-gate.sh --study legacy|full DIR` answers one question: does `cover` give every mutant the class the judge of today gives it? `DIR` is new or empty (anything in it is exit 2 before anything runs, and so is a `REF` that is not `legacy` or `full`). It runs the whole judge twice, in this order:

1. `QG_SELECT=REF` with `QG_KEEP_OUT=DIR/ref`. That run **is** the verdict of the reference: with `QG_BASE=<the base of the release>` and `REF=legacy` it is exactly the quality-gate of that release, so the study costs one judge run plus the one of `cover`. Its outcomes stay in `DIR/ref/<REF>/outcomes.json`. If it leaves none (the judge was refused, or the diff holds no mutant), the study stops there with exit 2 and the run of `cover` is not launched.
2. `QG_SELECT=cover` into a keep directory of its own, whose `cover/` becomes `DIR/cover` (the plan, the three passes and `merged.json`). If `cover` turned itself off and ran as `full` there is no `merged.json`, and that is exit 2, never «identical».

Then `mutants-gate.sh --compare-classes DIR/ref/<REF>/outcomes.json DIR/cover/merged.json` compares the class of **each** mutant (not the totals: a caught that turns missed and a missed that turns caught leave the totals equal), the set of names in both directions and the test command of each; `DIR/study.tsv` has one row per mutant (`name`, `ref`, `cover`, `pass`, `k`, `ref_test_s`, `cover_test_s`: the class in each run, the pass and the size of the subset of the cover run, and the seconds of its test phase with three decimals, `-` when it had none). The last two lines are `study: reference exit <n>` (printed before the run of `cover`) and one of

- `study: <n>/<n> identical — Q2 may be adopted`;
- `study: <k> differ — Q2 is NOT adopted`, after a `FAIL: class changed for <mutant>: <ref class> -> <cover class>` per difference.

The exit is 0 only if the reference is green and nothing differs; 1 if the reference is red or some class differs (a difference is red whatever the reference said); 2 if the study could not be run or compared. `cargo-mutants` 27.1.0 writes the class of a mutant whose test timed out as `Timeout`, and the comparator counts that name, so a timeout of the reference that `cover` catches is one difference and not a file whose totals do not add up.

**Criterion of acceptance.** `N/N identical` with `N` the number of mutants `cargo mutants --list --json` gives with the filters of the diff, zero mutants that change class, and the set equal in both directions. Any difference means Q2 is not adopted. Each one is diagnosed by running that mutant alone with the invocation of each mode and `--re '^<name>$'` (`study.tsv` gives its pass and its subset), to tell an unstable test from a failure of the selection; it is written down and never retried until it agrees. The change of the default to `cover` is a one-line commit (and its contract) in the next batch, with its `todo` (D-F6q).

**The audit** is `quality-gate.sh --study full DIR` over the head of the integration branch: it measures the clock of `full` and compares it with `cover` on every run. The timer that launches it (`systemd --user`) is a separate piece, and its hour is decided: Sundays at 02:00 (Leandro, 2026-11-09; «Weekly audit of the mutation judge» below has the two units). On 2026-11-10 the units are not installed yet; they are installed after 0.31 is published.

## Time per test binary

0.30 took `v029_accounts_over_real_http` from 553 s to 10.9 s and `v042_health_says_what_degraded` from 31 s to 1.5 s by hand, and nothing stopped them from drifting back. `scripts/test-times.sh` is the last step of `merge-gate.sh` and pins every binary's time, in both directions.

- **What is measured.** The gate's own log (`~/.cache/cuba-gate/merge-gate.log`), from its start to the line `=== end of timed steps ===`, which the planner prints before the first untimed row (`crap-gate`, then the mutation step: class `untimed` in `scripts/gate-dag.txt`): every `cargo test` of `run-all-tests.sh`, and nothing of the untimed rows (the build without docs, row `DC`, is only linted and prints no `finished in`). Each binary is a `Running …` (or `Doc-tests …`) header and the first `test result: … finished in X.XXs` after it; its time is the sum over every call that ran it. `tests/<name>.rs` is `<name>`; unittests are `lib:<crate>` and `bin:<crate>` (the library and the binary of this crate share the stem `memory_industry`); doc-tests are `doc:<crate>`. Windows (`tests\x.rs`, `.exe`, CRLF) and Linux give the same keys. The instrumented run of `crap-gate.sh` and the mutation step are not timed.
- **Why text.** `cargo test` on stable has no machine-readable timing: libtest's `--format json` needs `-Z unstable-options`, and `nextest` cannot be the verdict here (it splits a binary into processes, and this crate keeps global state behind a `Mutex`). The summary line of libtest is the one stable source, and the reader refuses anything it cannot pair: a header without a result before the next header or the boundary, a log without the boundary or without a single timed binary, are exit 2.
- **The ceilings.** `scripts/test-time-ceilings.txt`, one line per binary and host: `<os> <binary> <floor_s> <ceiling_s> <owner> <expires> <reason>`. A host (`windows` under Git Bash, `linux`) is judged by its own lines: the CPU-only Linux judge runs the reranker in 213 s and Windows on its GPU in 6 s, so one number for both would let a 30x regression on Windows through. A binary with no line has a ceiling of **10 s** and no floor: every regression 0.30 fixed measured more than 10 s on both hosts, and the slowest binary with no line measures under 7.
- **Two ways.** Over the ceiling is red. Under the floor is red too: the line no longer describes the binary, and has to come down with it. The floor is half the fastest run and the ceiling 1.3 x the slowest (1.5 with fewer than three complete runs on that host), because the noise is per binary: 1.05x for `v029_accounts_over_real_http`, 2.9x for `v029_bridge_against_the_real_daemon`, 3.2x for a test that waits on a generative LLM over the network. One factor wide enough for the noisy ones would leave the steady ones loose.
- **One line that is a bound, not a measure.** `v016_extract_without_sampling` calls the real CLI of a generative LLM, and the row that runs it (D3) names `CUBA_HANDLER_TIMEOUT_SECS=60`, so the extraction budget of the product, 60 % of it, is 36 s there and 18 s in production (CI-E14, a decision of 2026-11-10: in 17 runs the call took 7.5 to 18.3 s, and the one over 18 s was a red of the network, not of the code). Its ceiling is 1.1 x that budget until three runs with the 60 give it a line by the rule.
- **Dead and expired.** A line whose binary this host's run did not run, or whose `rust/tests/<name>.rs` no longer exists, is red; so is one past its date, one that is malformed, one whose ceiling is not above 10 s and one repeated. Every red of a run is printed together, after the table of the 10 slowest, which is printed green or red.
- **Before moving a line**, compare with the previous run of the same host and look at the machine's load: a run that shared the machine with another heavy job is not a measure (on 2026-10-13 a ceiling of 60 s in Mapupita-Rust read 73 s under load and 41 s alone). Then set the line by the rule above, with the run's numbers in its reason.
- **Order.** It runs last, after `mutants-gate.sh`: a time red is drift, not broken behaviour, so the same run still reports codigo-muerto, crap-gate and mutation instead of stopping twenty minutes earlier. It waits (at most 30 s) for the boundary line to reach the log through `tee` before reading it.

Replay any log without cargo:

```bash
./scripts/test-times.sh --judge <log> --until '=== end of timed steps ===' [--os windows|linux] [--today YYYY-MM-DD]
./scripts/test-times.sh --self-test
```

## Test counts per section and per binary

Every `cargo test` of `run-all-tests.sh` goes through `run_counted`, which counts the tests the call ran (passed + failed over its `test result:` lines, read by `test-times.sh --count`) and holds the number against `scripts/test-section-counts.txt`. A section that ran more or fewer tests than its line says is red: a test that stops being run is the same silence as one that is skipped.

- **Two shapes of line.** A section states either one total, `<section> <tests> <motive>`, or one line per test binary, `<section>/<os>/<binary> <tests> <motive>`, never both. `<os>` is `linux` or `windows`, and a judge reads only the lines of its own system; a section with lines per binary and none for that system is red. `<binary>` is the key of the test times: the file name for `tests/<name>.rs`, and `lib:`, `bin:` or `doc:` plus the crate for the rest.
- **The pass without `--ignored` is counted per binary**, in three sections: `plain` (the library and the test files that are not contracts of the judge, lane `D1`), `contracts` (the files of `JUDGE_CONTRACTS`, lane `CT`) and `doc` (the doctests, `doc/<os>/doc:memory_industry`, lane `CT`). Every binary of that pass is in exactly one of the three, and a contract holds the file to that: a test file with no line, a line whose file is gone and a file counted twice are red in milliseconds, not at the end of a judge. The `--ignored` and release sections state a total.
- **The two `[[bin]]` carry `test = false`** (CI-F5c, D-F5w). `memory-industry` and `cuba-memorys` are two targets of one file, `src/main.rs`, which has no tests and no `mod x;`; each used to get a test harness that compiled the file and ran 0 tests, in `D1`, in `DC` and in the clippy of `CL` and `DC`. With the flag `cargo test` with no filter, `--bins` and `--all-targets` do not build those harnesses; the binaries are still built for the integration tests (`CARGO_BIN_EXE_memory-industry`, `CARGO_BIN_EXE_cuba-memorys`). For the judge it means: no `bin:memory_industry` or `bin:cuba_memorys` line in the test log, so none in `test-section-counts.txt` and none in `test-time-ceilings.txt`; two guards in `local_gate_contract.rs` keep it so: every `[[bin]]` that shares `src/main.rs` must say `test = false` (G1), and a bin with the flag cannot hold a `#[test]` or an out-of-line `mod x;`, because that code would never run (G2). Cargo's warning that the file is «present in multiple build targets» stays (D-F5y): it comes from reading the manifest, before the flag is looked at, and removing it would need two source files, which this batch does not do.
- **Both ways and all at once.** A binary that ran and has no line, a line whose binary did not run and a number that differs are each named, in one run. Every section that runs tests has its line, and every line is a section that runs.
- **The line of a binary changes in the same commit as its tests**, by what the diff of that commit adds or removes in that binary. A number is never copied from a red run: a difference the diff does not explain is a test lost or gained, not a new number.
- **`--check-counts`.** `bash scripts/run-all-tests.sh --check-counts SECTION LOG` judges a cargo log that was saved against the lines of a section, with the function the judge itself uses. It opens no database, takes no lock and runs no cargo: 0 green, 1 red, 2 when it cannot read the log.

## Advisory exceptions

Until 0.31 the project ignored advisories in two lists that did not agree: `rust/.cargo/audit.toml` held four, `rust/deny.toml` one, none had an owner or a date, and one of them (`RUSTSEC-2026-0002`, lru) had matched nothing since lru 0.18.5. Now there is one list, `scripts/advisory-exceptions.txt`, one line per advisory: `<advisory> <owner> <expires> <reason>`.

- **Why a text file, not a TOML.** cargo-audit 0.22 takes only bare IDs in `ignore` and rejects any other key (`unknown field 'owner'`); cargo-deny 0.20 takes only `id` and `reason` (`unexpected-keys`). The owner and the date fit in neither, so the two TOML lists are mirrors of the IDs, and the reason is written once, in the text file.
- **When it is judged.** Right after `cargo audit`, before `cargo deny`: `cargo audit --json --no-fetch -f rust/Cargo.lock` again, from a temporary directory so that no `audit.toml` applies, gives every advisory the lockfile carries, ignored ones included (0.42 s measured). `scripts/advisory-exceptions.sh --judge <report>` reads it. The exit of that call does not count: without ignores it finds the excepted advisories and exits 1, which is the point. A report that was made with ignores anyway (a `~/.cargo/audit.toml`) is exit 2: every excepted advisory would look unused.
- **Red, exit 1, all at once:** a line that is malformed or repeated; an expiry in the past (the day of `expires` is still green) or more than 180 days ahead; an advisory the report does not carry, in `vulnerabilities` or in `warnings` (an unmaintained crate counts); an ID that one mirror ignores and the list does not, or the other way round, a crate spec in `deny.toml` included.
- **Exit 2:** no report, one that is not `cargo audit --json`, a list or a TOML that is missing or does not parse, a Python without `tomllib` (3.11+).
- **`cargo deny check advisories` by hand** says `advisory-not-detected` for the three current exceptions. cargo-deny resolves features (`sqlx-mysql` and `sqlx-sqlite` are never built) and, with `unmaintained = "workspace"`, does not report unmaintained crates deep in the tree. cargo-audit reads the lockfile and sees them, and it is the judge.
- **To add one:** the line in the text file with your name, a date and why it does not reach the binary and what ends it; the ID in both TOML files. To renew one, a new date and a reason that says what changed. To drop one, remove it from the three places.

    ./scripts/advisory-exceptions.sh --judge <report.json> [--today YYYY-MM-DD]
    ./scripts/advisory-exceptions.sh --self-test

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

## Lanes of run-all-tests.sh (CI-F3.3a)

`scripts/run-all-tests.sh` is split into one function per lane: `lane_CL` (fmt and clippy), `lane_B1`
(`cargo test --no-run`, which builds every test binary of the debug target once, and the debug binary of
the database), `lane_D1` (the throwaway database and the plain pass: the library and the files of `tests/`
that are not contracts of the judge), `lane_CT` (the eight contracts of the judge and the doctests),
`lane_D2` (the `--ignored` tests and the admin-role and embedding ones), `lane_D3` (the generative-LLM
tests), `lane_NLI`, `lane_B3` (the release build and its freshness check), `lane_R` (the reranker
contracts) and `lane_E` (the E2E, the doctor, the MCP live session and the eval smoke).

- Without arguments the lanes run in the order of `DAG_LANES` in the script, which is the `idx` order of
  `scripts/gate-dag.txt`. That is the serial judge.
- `scripts/run-all-tests.sh --lane <id>` runs only that lane. Several `--lane` in one call run in the
  order given, in one process and with one database (CI-F3.3b-1). A missing id, or one that is not in
  `DAG_LANES`, anywhere in the list, exits 2 before the lock is taken, before any lane is made and
  before cargo runs.
- `merge-gate.sh` hands the table to the planner, `scripts/gate-dag.py`, once its preamble has run (the lock,
  the server check, the sweep). The planner runs the thirteen rows of `scripts/gate-dag.txt`:

  | Row | Waits for | What it runs |
  |---|---|---|
  | `P` | nothing | the cheap steps: `cargo audit`, the advisory exceptions, `cargo deny`, `cargo fmt --check`, `codigo-muerto`, the npm smoke and the models check |
  | `B1` | `P` | lanes CL and B1: fmt, `clippy --all-targets`, `cargo test --no-run` and the debug binary of the database. The only row that compiles the debug target |
  | `DBG` | `B1` | lane D1, the plain pass: the library and the test files that are not contracts of the judge |
  | `CT` | `B1` | lane CT: the eight contracts of the judge and the doctests; no database, no models |
  | `D2` | `B1` | lane D2: the `--ignored` sections, the admin-role tests and the embedding model |
  | `D3` | `B1` | lane D3: the tests that need a generative LLM |
  | `NLI` | `B1` | lane NLI, alone (`exclusive`) |
  | `B3` | `P` | lane B3: the release build and its freshness check |
  | `REL` | `B3` | lane R: the reranker contracts on the release build |
  | `E2E` | `B3` | lane E: the E2E, the doctor, the MCP live session and the eval smoke |
  | `DC` | `P` | clippy of the build without docs, no tests |
  | `CV` | `P` | `crap-gate.sh` |
  | `M` | `P` | `mutants-gate.sh` |

  Each row is `merge-gate.sh --step <id>`, a process of its own, with a cluster of its own when it names
  one, and the work of a row is its `step_<id>()` function in merge-gate.sh. Between processes only files
  cross: the readers of `B1` find its test binaries in their target, and `REL` and `E2E` find the release
  binary of `B3` by its path.
- `B1` is the only row that compiles the debug target. `DBG`, `CT`, `D2`, `D3` and `NLI` are its readers
  (`reader` in the table): each waits for `B1` and for nothing else, has a cluster of its own, and runs
  `cargo test` over what `B1` built, with one cargo job and exactly the profile tokens of `B1` (the profile is
  in the hash of every unit, so another one would build the target again). A reader compiles nothing, and
  the judge holds it to that: a `Compiling` line in the log of a section that counts tests is red (`cargo
  compiled <unit> in a section that runs tests`), and so is a unit that is not `fresh` when a lane asks cargo
  for the debug binary to provision its database, before the database is made. A unit compiled again would be
  written over a binary that the row beside it may be running. Readers do not queue behind cargo's lock
  either: measured on 2026-11-10 with two `cargo test` over one warm target, the second reached `Running`
  6.1 s after the first and ended 62 s before it, with no `Blocking waiting for file lock` line.
- `CT` runs the files of `JUDGE_CONTRACTS` in run-all-tests.sh (`ci_contract`, `gate_dag_contract`,
  `gate_history_contract`, `gate_usl_contract`, `group_no_server`, `local_gate_contract`,
  `packaging_contract` and `v047_without_a_database_url_the_judge_reaches_no_default`) and the doctests.
  They judge the scripts and the files of the tree, and what they need of a server or a planner they make
  themselves in a throwaway directory: none reads `DATABASE_URL` or calls the helpers that make a database
  (a contract holds that), and none loads a model. So the row names no `models`, its lane makes no
  `brain_gate`, and it does not wait behind the rows that load models. It still names a cluster, for the
  reason `NLI` does. The split is made by name and never with `--skip`: every file of `tests/` runs without
  `--ignored` in exactly one of `D1` and `CT`.
- `NLI` is an `exclusive` row: it asserts a clock (`nli_entailment` under 500 ms), so the planner starts it
  after every row of lower idx and runs nothing next to it. It is a reader of `B1` like the other four. Its
  tests open no database, but `run-all-tests.sh` runs only against a judge's server, so the row has its own
  cluster like the others (CI-J12).
- `E2E` waits for `B3` only (CI-E9). Its lane compiles nothing and reads nothing that a debug row makes: it
  provisions `brain_gate` in its own cluster with the release binary `B3` left, already held against the
  sha256 `B3` recorded, so the database is migrated by the binary the E2E judges, and the row can start while
  `DBG` still runs.
- A row runs only with the ticket the planner hands it (`CUBA_GATE_LANE_TOKEN`): `--step` checks the ticket
  without spending it, and the lane it starts spends it. Run by hand without one, `--step` exits 2 and takes
  no lock. The verdict is the serial one in every mode: the red row of lowest idx wins, the rows before it
  finish and the rows after it are stopped. The boundary line `=== end of timed steps ===` is printed by
  the planner before its first untimed row (CV), and `test-times --judge` reads the log up to it.
- `scripts/gate-dag.txt` is the table of the steps: `<idx> <id> <deps> <recursos> <clase> <cpu>
  <ram_gb> <dur_s> <orden…>`. It has one reader, the planner (CI-J18): `gate-dag.py run --table <file>` runs
  it, and `gate-dag.py rows --table <file>` says what `run` would run, a line `row <id> <recursos, or ->
  <order without its NAME=value>` per row in idx order and a line `overlap <a> <b>` per pair of rows that can
  be alive together (neither is `exclusive` and neither waits for the other). `gate-audit.sh` asks `rows`
  and `gate_usl.py` calls the planner's parser, and neither reads the file again. A table that `run` refuses
  is exit 2 from `rows` too, with the same `FAIL:` line and nothing on stdout.
- The grammar of a row is checked before anything starts: an idx of `[1-9][0-9]*`; an id and a target of
  `[A-Za-z0-9_]+`, which end up in paths; a cluster of `C_[a-z0-9]+`, whose lane is its name; a class of
  `normal`, `exclusive` or `untimed`; `cpu`, `ram_gb` and `dur_s` finite and above 0; and no word that
  starts with `#` (a comment goes in a line of its own). The order may be preceded by `NAME=value` tokens,
  the environment of the step. Only the names of `ROW_ENV` in `gate-dag.py` may be written there
  (`CARGO_BUILD_JOBS`, `CARGO_INCREMENTAL`, `CARGO_PROFILE_DEV_DEBUG`, `CARGO_PROFILE_DEV_STRIP`, the three
  `CUBA_*_INTRA_THREADS`, `MUTANTS_JOBS` and `CUBA_HANDLER_TIMEOUT_SECS`), none with an empty value, and a `*_JOBS`, `*_THREADS` or `*_SECS` is a
  whole number above 0. `PLANNER_ENV` (`TMPDIR`, `CARGO_TARGET_DIR`, `CUBA_GATE_LANE_TOKEN`) is what only the
  planner sets, and a step that would differ from the planner's own environment in any other variable is
  not started. The table as a whole is judged as well: a cluster belongs to one row, two rows in one target
  are ordered by their deps unless both are readers, and a reader has above it the row that builds its target.
- The history of a table is keyed by its rows: the sha256 of the rows with their spaces collapsed, without
  comments or blank lines, sorted. A comment or another order of the lines keeps the history; a number of a
  row starts a new one.
- A test of the contracts reads the table and checks its thirteen rows, their deps and classes, that every
  starting duration is above 0, that every row's order is its `--step`, that every `--lane` of a row is a
  function of run-all-tests.sh, that `DAG_LANES` is those lanes in `idx` order, that the graph has no cycle
  and that `REL` and `E2E` depend on `B3`, which builds the binary they read.
- The lanes assume their dependencies have run: `--lane D1`, `CT`, `D2`, `D3` and `NLI` run the test
  binaries `B1` built, and `--lane R` and `--lane E` need the release binary of `B3` on disk (they recompute
  `REL_BIN` and `REL_SHA` from it). The planner enforces that order through the deps of the table; run by
  hand, a lane that finds its producer missing fails and names the lane to run first. `brain_gate` is made
  by `ensure_gate_db` in whichever lane needs it and does not find it: `D2` and `D3` with the debug binary
  of `B1`, `E` with the release binary of `B3`. In one process (a run with no `--lane`) `D1` makes it and
  the rest find it.
- `lane_D2`, `lane_D3` and `lane_E` each export `DATABASE_URL` as `GATE_DATABASE_URL`, the URL of
  `brain_gate`, so each lane reaches it on its own.

## Targets per tree and the DAG switch (CI-F3.3c)

- A row of `scripts/gate-dag.txt` that names `target:<T>` builds under
  `CARGO_TARGET_DIR/<tree key>/<T>`, where the tree key is the name of the tree's directory and 12 hex of
  the sha256 of its realpath. The directory writes that realpath in `.gate-tree`. A target whose
  `.gate-tree` names another tree is refused with exit 2 before cargo runs, and the step does not start.
- The rows that compile have one target each: `B1` `T_dbg`, `DC` `T_docs`, `CV` `T_cov`, `M` `T_mut`,
  `B3` `T_rel`. The readers `DBG`, `CT`, `D2`, `D3` and `NLI` read the `T_dbg` that `B1` builds, and the
  release readers `REL` and `E2E` the `T_rel` that `B3` builds.
- A step without a `target:` keeps the `CARGO_TARGET_DIR` of the gate.
- `GATE_DAG` chooses how the planner runs the one table. With nothing set (the default since CI-E24,
  2026-11-10) it asks for `auto`, the DAG. `0` or `serie` forces the series: the rows in idx order, one at a
  time. `1`, `maximo` and `auto` are one and the same DAG: a row starts when its deps ended green and the
  admission below lets it in. The switch is the environment of `gate-dag.py`; `merge-gate.sh` does not set
  it. Measured on 2026-11-10 on the 24 threads of this machine, with the crate recompiled: the series in
  991 s, the DAG in 387.5 s alone and in 583 s in a shared turn of 12 CPUs.
- The DAG cannot be the default while a line of `scripts/gate-latency.txt` is `pendiente`:
  `the_dag_is_not_the_default_while_a_latency_line_is_pending` (D-F34n). Going back is one word: `"auto"` to
  `"0"` in `gate-dag.py`.
- A DAG, asked for or by default, still runs in series, and a `note:` line says why, when the scope the gate runs
  in is not delegated (nothing could be measured or killed per row), when the history has no green serial
  run of this table (so a table whose rows changed starts in series), when `DATABASE_URL` names a server of
  the operator (every row would share it, and sqlx drops databases by name), or when the machine could not
  be measured (then with one job). The run is recorded with the mode that ran. So the first run of a table, every run
  with an operator's `DATABASE_URL` and every run outside a delegated cgroup (outside `pesado`, and on Windows) is a
  series.
- Before the first row the planner measures the machine, with the same count `gate_cargo_jobs` makes, and
  prints its plan in one line: `plan: nproc=… cgroup_cpu=… ram_free=… GiB disk_free=… GB battery=… jobs=N
  (gate|operator|failed) asked=<serie|auto|maximo> runs=<serie|dag>`, followed by the reason in
  parentheses when a DAG that was asked for runs in series and by `failed=<what>` when a measure could not
  be read. The disk and the battery are only measured: they change no decision.
- In the DAG a ready row is admitted by what the machine has left (CI-E7a). `C` is `min(nproc, the CPUs of
  the cgroup)` and `R` is the free RAM less the larger of 4 GiB and 10 %, both from the numbers of the
  `plan:` line. An `exclusive` row starts alone, after every row of lower idx, and nothing starts beside it;
  past that, with nobody alive a row always starts. Otherwise it waits, in this order: for `models`, while
  two rows with models are alive or `MemAvailable` is under 4096 MiB plus its own estimate; for `cpu`, while
  the cores of the live rows plus its own pass `C`; for `jobs`, while the cargo jobs of the live rows plus
  its own pass `N`, the jobs of the machine; and for `ram`, while what the live rows hold plus its estimate
  passes `R`. Ready rows are tried in the order of their critical-path rank.
- The planner picks up what it makes (CI-E15). Each run has a directory `memory-industry-dag.<suffix>` under the
  judge's `TMPDIR`, with `pasos/` (the log of each row), `tmp/<row>` (the row's own `TMPDIR`) and `planner.pid`. A
  green row's `tmp/<row>` goes when the row ends; the whole directory goes when the run ends with exit 0 and a
  verdict. A red run, or one with no verdict, keeps it and prints `dag: run directory: <path>` after its summary.
  Before it makes its own, a planner deletes the siblings of more than seven days whose `planner.pid` no longer
  names a live process. A row that ends with processes still in its cgroup gets them killed and named in a note:
  `note: paso-21-CT ended with orphan processes in its cgroup (sleep x12); they were killed`. It is a note and not a
  red: a child of a second that outlives its row would be a red by a race.
- The jobs of the rows that are ready when `P` ends have to fit together (CI-E14): `B3` names 12, `B1` 8 and `M` 4,
  the 24 threads the table was written for. With `M` at 8 the green DAG run of 2026-11-10 kept `B1` waiting 180 s for
  the reason `jobs` on a machine at 8.5 of 24 cores; `M` builds one crate per mutant and its width is `MUTANTS_JOBS`,
  which is also its `cpu`. A contract adds the three up, so a row that asks for more says who waits.
- A row with `models` that ends green is held against the `resource plan` line its binary wrote (D-F34b). The pools
  it has to show are the ones that plan's own CPUs give (CI-E13, D-E13a): half of the line's `cores_logical`, at
  least 1 and at most the number the rows name; a line without `cores_logical` is held against the table as it is,
  and the reranker on the GPU is not compared. So the verdict is the same in a turn of 12 CPUs as in one of 24.
- The cores, the memory and the duration of a row are the `cpu`, `ram_gb` and `dur_s` of the table until
  the history has green serial runs of that table. Then the duration is the median of the last five, and
  the cores and the memory are the p95 of what those runs used, the memory with a margin of 1.25.

## What a run records (CI-E4)

The planner writes every run to the history, under
`${CI_TIEMPOS_DIR:-$HOME/.cache/ci-tiempos}/memory-industry/<UTC day>/`: one line per row in `pasos.jsonl`
and one per run in `corridas.jsonl`. `scripts/gate_history.py` is the only writer and never changes a
verdict: a history that cannot be written is one `history:` line on stderr.

- When the rows end the planner prints `dag: T=…` (the clock of the run against its lower bounds), then
  `dag: resources: cpu_s=… cores=… mem_peak=… at_t=… current=… anon=… shmem=… cache=… kernel=… samples=…
  serie_final=… tail=… serie_share=…`, then one `dag: step <id> seg=… wait=… overlap=… mem_peak=… cpu_s=…
  cores=…` per row that ran. `n/a` is a number that was not measured.
- `nucleos`, of a row and of a run, is the mean cores it used: its CPU seconds over its seconds (over `T`
  for the run). The CPU of a row is its own cgroup's, and the CPU of a run is the sum of its rows', null
  while one of them is null.
- `mem_peak` of a run is the `memory.peak` of the scope the gate runs in, read through a descriptor of the
  planner's own that is zeroed when the planner starts: a peak read by path would be the one since the scope
  was born, the preamble of the gate included. `mem_peak` of a row is the peak of that row's cgroup.
- The kernel keeps the mark of the total and not its parts, so `mem_muestra` of a run says what the memory
  was made of at the highest `memory.current` the planner sampled (at the start, every second, at the end
  of each row and at the close): `current`, and `anon`, `file`, `shmem` and `kernel` of `memory.stat`, with
  its time `t` on the clock of the run and the number of samples, `muestras`. `cache` in the printed line is
  `file` less `shmem`, the page cache that can be reclaimed. What happened between two samples is not known,
  and the cache another job loaded first is charged to that job and not to this scope.
- `serie_final_s` is the seconds the run spent with exactly one row alive while another was ready and had
  not started, plus `serie_cola_s`, the seconds with one row alive after the last row had started (the
  tail). One row alive while the others wait for their deps is the critical path and does not count.
  `serie_share` in the line is `serie_final_s` over `T`.
- What was not measured is written as null, never 0, and every null of a field the planner measures has
  its reason: `sin_medida`, `{field: reason}`, in the record, and a `note: not measured (<fields>):
  <reason>` line in the output. A scope that is not delegated is not measured at all: every number of the
  run is null with that reason.

## Calibrating the jobs of a row (`gate_usl.py`)

`python3 -I scripts/gate_usl.py <order>` fits the Universal Scalability Law to the builds of the judge and
proposes the `CARGO_BUILD_JOBS` of the rows that compile. It edits nothing: `recomendar` prints a diff of
`scripts/gate-dag.txt` for the orchestrator to review and commit, because a number of a row is in the key of
the history.

- `barrido --perfil debug|release` compiles what one row compiles, cold and without incremental, at several
  `CARGO_BUILD_JOBS`, and writes a point per level and replica to `usl.jsonl` in the history. There are two
  profiles, one per row that compiles a target the tests run: `debug` is `B1` and `release` is `B3`. The
  sweep takes the profile tokens and the target of its row from the table: the profile is in the hash of
  every unit, so without them it would measure another build and leave a second set of artifacts in the
  judge's target. `CARGO_TARGET_DIR` has to be set, as for the judge. The levels are fractions of the jobs
  of the machine as the planner counts them. A grid of fewer than five levels, or a machine that is busy
  (CPU pressure `some avg10` above 15 %), is exit 2 and nothing is run.
- `ajustar --perfil P` fits the curve to the last sweep of a profile. Exit 3 when the fit is not reliable,
  with the first criterion that failed.
- `recomendar` gives the slack of each row and the jobs that a simulation of the DAG likes best, and it says
  what it assumes, in a line that starts with `supone:` (and `"supone": "dag"` with `--json`): the DAG with
  the planner's own admission, up to the jobs of the machine, and no contention. In series the clock is the
  sum of the rows, which it does not calibrate. Exit 3 when a sweep or a cold serial run is missing, with
  the order that makes it.

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
- It writes `~/.cache/cuba-gate/receipts/<full sha>` only when that commit is all that was judged: no path that differs from `HEAD`, tracked or untracked, outside the docs-only list (`scripts/recibo-lista-blanca.txt`), when the gate starts **and** when it ends (`scripts/recibo-vale.sh --dirty`), and `HEAD` where it started. The file is the four lines the tag carries under its title: `local-gate: MERGE GATE PASSED <sha>`, `local-gate-date:` (UTC, when the gate passed), and the `kill_rate=` and `MERGE GATE PASSED` summary lines.
- A dirty tree, a tree or `HEAD` that changed during the run, or an unclean log: no receipt, and the run says why in a `no receipt:` line. A dirty start is announced at the start too, not an hour later.
- A run that starts clean on a commit removes that commit's earlier receipt first: a red run on it takes the pass away, a green one writes it again.
- `release.sh` uses a receipt only if it is named after `HEAD` **and** its four lines are that shape for that sha. A copy of another commit's receipt under `HEAD`'s name runs the gate.
- A gate that starts over such a tree writes `receipts/<sha>.started` and removes it only when it ends green (CI-E29): a red run, a signal or a kill leaves the mark, and `scripts/recibo-vale.sh` looks at the mark before the receipt, so a red on the same code wins over an older pass.
- Every run ends by writing `.gate/ultima-corrida.json` in the tree it judged (ignored by git): the exit code, `HEAD`, and the fingerprint of the judge. The stop hook reads it. When the judge ran in another tree than the working one, `bash scripts/recibo-vale.sh --write-pointer` in the working tree writes it from the receipt it has just judged; if that is forgotten the hook blocks a «green» twice.
- `bash scripts/recibo-vale.sh` says whether a green receipt still holds for `HEAD`: it walks the first parents (200 at most) to the nearest mark or receipt, and the receipt holds only if every path changed since, and every untracked one, is documentation by `scripts/recibo-lista-blanca.txt` (`backlog/*.md`, `docs/*.md`). A path under `scripts/` or `rust/` is never documentation, whatever the list says; a `.md` the judge reads (`docs/gate.md`, `AGENTS.md`, `CHANGELOG.md`, `README.md`) is not documentation, and a contract holds the list to that; changing the list invalidates the receipt. `--is-doc <path>` says why a path is or is not; a `git` that fails is «could not check», exit 2, never «holds».
- `release.sh` tags the judged commit when only documentation came after it; with a mark on `HEAD`, or code after the judged commit, it runs the gate on `HEAD`.

The receipt is a file in your home, not a signature: anyone who can write there can write one, as anyone who can push a tag could before. It saves a run; it does not add a judge.

`publish.yml` no longer asks GitHub whether `ci.yml` passed. Its first job fetches the annotated tag (`actions/checkout` leaves a lightweight ref in its place, [actions/checkout#290](https://github.com/actions/checkout/issues/290)) and runs `scripts/release.sh --verify-receipt <tag> <sha>`: the tag must be annotated, point at the commit being built, and carry the receipt line for that commit. Every other job needs it. A tag pushed by hand is refused, so the only way a release reaches npm, PyPI and the binaries is through a green local gate.

`ci.yml` runs only by `workflow_dispatch`. Its `release-matrix` job, which compiles the five published targets, no longer runs on its own: the local gate does not check other platforms, so dispatch it by hand before a release that touches features or platform code.

`./scripts/release.sh --self-test` drives each guard against a throwaway repo with a stand-in `merge-gate.sh`, and checks that the receipt it writes is the one `--verify-receipt` accepts. It also checks that a receipt for `HEAD` replaces the gate and its lines reach the tag, and that a receipt for another commit, one naming another commit inside, and `--rerun-gate` all run the gate. `./scripts/merge-gate.sh --self-test` checks the other half: a receipt after a green run over a clean tree, and none (with the reason) after an untracked file, a tree or `HEAD` that changed during the run, or an exit 0 over `SKIPPED`.

## Programming rules (this repo)

Versioned under `.cursor/rules/` (never a junction to `~/.cursor/rules`). Six-pack + TDD + two judges. Comments stay. Plant rules (Playwright, React Query, guardian-planta) are not copied here.

Handoffs: `.cursor/handoffs/*.yml`, validated by `scripts/validar-handoff.sh` — shape, plus the two-pass protocol: `commit` has to exist in the repo and `tests: written|frozen` (required when `commit != none`) is checked by extracting the test regions at that commit and in the tree: the `#[cfg(test)]` regions of a `.rs`, a whole `.rs` under `tests/`, and in a `.sh` the lines between `# --- self-test begin ---` and `# --- self-test end ---`. A gate script whose `--self-test` dispatch sits outside a marked region, or whose markers do not pair, is refused whatever `tests` says. `written` fails when none moved, `frozen` when one did. An optional `paths:` list limits that comparison to the handoff's own slice, so that one agent's honest green pass is not refused for a test another agent edited in the same tree; it is refused empty, with a path that exists neither in the tree nor at `commit`, and with `commit: none`. `scripts/validar-handoff.sh --self-test` runs a fixture against each guard.

Two handoffs are also judged by what their tests cost (CI-E6), after `written` and `frozen` and with no new field. From the specifier to the implementer, `task` or `evidence` names its `.cursor/handoffs/<name>.spec.md`, beside the `.yml`, and that spec carries a cost table with the header `| Escenario | Nivel | Segundos | Carril | Por qué no el barato |` and one row per `Escenario:` of its gherkin blocks, in both directions: the level is `pura`, `guion`, `servicio` or `e2e`, the seconds are a number, the lane is a row of `scripts/gate-dag.txt`, and with a level that is not `pura` the last cell says why the cheap one does not prove it. From the implementer, with `type: git_handoff` and a commit, `evidence: |` carries, each on an indented line of its own, a `segundos: <S> s; <bounded command>[; <its last line>]` per command that ran the new tests (when the line quotes cargo's `finished in Xs`, X has to be S: the number comes from the measured line, not from memory) and, if the slice touches `scripts/`, a `carril: <row id>; comparte: <resources of that row, or nada>`. The handoffs written before the rule pass through `scripts/handoffs-sin-coste.txt`: one line per handoff, `<sha256 of its content without CR> <name> <expiry> <owner> <reason>`. Handoffs are not versioned, so the content is the only identity a historic one keeps, and editing it makes it new. The list is closed (a contract of `local_gate_contract.rs` fixes how many lines it has and their latest expiry), a line past its date exempts nothing, and a malformed line turns red every judgement that reads the list.

## Weekly audit of the mutation judge (CI-F6, D-F6r)

`scripts/qg-weekly-audit.sh` runs `quality-gate.sh --study full DIR` on the HEAD of the integration branch, so that the equivalence of `full` and `cover` (D-F6p, D-F6q) is watched on the code as it changes and not only once. Leandro's decision of 2026-11-09: weekly, Sundays at 02:00. It is one run of the study, not a second judge: it does not replace `merge-gate.sh`, and nothing in the SIL waits for it.

What one run does:

1. Reads the HEAD of `QG_AUDIT_BRANCH` (default `main`) in the clone the script lives in. If it is the HEAD of the last audit that had a verdict, it says `unchanged` and exits 0 without taking the turn.
2. Otherwise makes a detached worktree of that HEAD under `<state>/work/<stamp>-<sha12>/` and runs, from inside it, `pesado -m 36G -w 3600 -n qg-auditoria -- timeout --kill-after=120 $QG_AUDIT_MAX_SECS bash scripts/quality-gate.sh --study full <DIR>` with `QG_BASE` set and no `DATABASE_URL`. The wait for the turn is an hour and the study has `QG_AUDIT_MAX_SECS` (default 21600, 6 h; the study is expected to take 1.5 to 3 h, E) once it has it: nothing waits without a deadline. While it runs it holds the lock of `pesado`, so no agent compiles.
3. Keeps `audit.log` (the output of the study) and `study.tsv` in `<state>/<stamp>-<sha12>/` together with a `verdict` file, and removes the worktree and the work directory, also when the study fails or the script is told to stop. The heavy part of the study (`DIR/ref`, `DIR/cover`) is not kept: a difference is diagnosed mutant by mutant from `study.tsv` (its pass and subset), as in the study's own procedure.

`<state>` is `${XDG_STATE_HOME:-$HOME/.local/state}/memory-industry/qg-audit`. Its `last` file holds `<sha> <verdict> <stamp>` of the last audit with a verdict.

| Exit | Meaning | `last` |
|---|---|---|
| 0 | `study: N/N identical`, or the HEAD is unchanged | moves (unless unchanged) |
| 1 | the classes differ, or the reference is red | moves: the week has its answer |
| 2 | no verdict: the study exited 2 or died, it did not finish in `QG_AUDIT_MAX_SECS`, or the audit could not set up (the branch or the base is not there, no base) | stays, so the next run tries again |
| 75 | `pesado` stayed busy for the whole wait | stays |

`QG_AUDIT_BASE` is the `QG_BASE` of the study, the ref whose diff to the HEAD is mutated. Without it, the base is the HEAD of the last audit with a verdict, so each audit judges what changed since. The first audit has no earlier one and exits 2 asking for `QG_AUDIT_BASE`. Set it to judge every week against the same base (the 0.31 base gives its 787 mutants) instead of the changes of the week. A diff with no `rust/src` file judges no mutant, and the exit alone does not say how many were judged: read the `study:` line of `audit.log`.

Two `systemd --user` units run it. They live outside the repo, like `respaldo-memoria.timer`, and the PATH is set in the service because a user service does not read the login profile (`pesado` is in `~/.local/bin`, `cargo` in `~/.cargo/bin`).

```ini
# ~/.config/systemd/user/qg-auditoria-semanal.service
[Unit]
Description=Weekly audit of the mutation judge (quality-gate.sh --study full)
[Service]
Type=oneshot
Nice=10
Environment=PATH=%h/.local/bin:%h/.cargo/bin:/usr/local/bin:/usr/bin:/bin
# The first audit needs a base; name it here (a ref of the clone) and keep the line to judge always against it.
#Environment=QG_AUDIT_BASE=<ref>
ExecStart=%h/proyectos/mcp-servers/Memorys/scripts/qg-weekly-audit.sh
```

```ini
# ~/.config/systemd/user/qg-auditoria-semanal.timer
[Unit]
Description=Weekly audit of the mutation judge, Sundays at 02:00
[Timer]
OnCalendar=Sun *-*-* 02:00:00
Persistent=true
RandomizedDelaySec=300
[Install]
WantedBy=timers.target
```

```bash
systemctl --user daemon-reload
systemctl --user enable --now qg-auditoria-semanal.timer
systemctl --user list-timers qg-auditoria-semanal.timer      # the next Sunday
journalctl --user -u qg-auditoria-semanal.service -n 50      # the last audit
cat ~/.local/state/memory-industry/qg-audit/last             # <sha> <verdict> <stamp>
```

The service is `failed` after an exit 1, 2 or 75, which is how `systemctl --user --failed` shows a week that needs a look. `ExecStart` names the clone the script lives in: the audit reads `QG_AUDIT_BRANCH` from that clone's refs and the script has to exist in its checkout. `./scripts/qg-weekly-audit.sh --self-test` drives each of the rows above against a throwaway git repo with a stand-in `pesado` and a stand-in `quality-gate.sh`; it never takes the real turn and never writes under the real state directory.
