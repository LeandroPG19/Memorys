# Deploying MemoryIndustry for a LAN

One daemon, several machines. This page is the whole procedure; you should not
need to read `rust/src` to get a daemon up. If you do, that is a bug in this
page — say so.

Every knob named here is also in `.env.example`, with its default.

## 0. What you are about to expose

`serve` answers `POST /mcp`, and that endpoint reaches **the entire graph**:
every observation, decision and episode any client ever wrote. There is no
per-tool permission model on the LAN path. The bearer token is the only thing
between the graph and anyone who can route a packet to the port.

Two consequences, both deliberate:

- `serve` **refuses to start** on a non-loopback address without
  `CUBA_HTTP_TOKEN`. That refusal is a feature; do not work around it.
- The token travels in **clear text** over the LAN on every request. There is
  no TLS yet. On a switched office LAN that is a considered trade-off; on a
  shared or wireless network it is not. Use a long random token, and treat the
  port as something only the client machines should reach.

## 1. The database

`docker compose up -d db` brings up Postgres with pgvector on `127.0.0.1:5488`.
The daemon needs it; the client machines do not — they only talk to the daemon.

## 2. Models

    memory-industry models all

Downloads the embedder, the NLI model and the cross-encoder reranker into
`~/.cache/memory-industry/`. Without the reranker, searches still work — they
come back in RRF order, without cross-encoder reordering.

## 3. GPU, if there is one

Only the reranker benefits. The embedder is INT8 and the CUDA execution
provider has no kernel for `DynamicQuantizeLinear` / `MatMulInteger`, so it
runs on CPU no matter what you set; the NLI model is FP32 and not worth the
VRAM. Defaults are therefore `CUBA_EMBED_DEVICE=cpu`, `CUBA_RERANK_DEVICE=gpu`,
`CUBA_NLI_DEVICE=cpu`.

Build with the feature and point the runtime at the GPU libraries:

    cargo build --release --features cuda

`ORT_DYLIB_PATH` must name `onnxruntime.dll` / `libonnxruntime.so`, and the
CUDA execution-provider libraries must sit **next to it** and be reachable on
`PATH` (Windows) or `LD_LIBRARY_PATH` (Linux). `memory-industry models runtime
--gpu` fetches the provider libraries as well as the main one.

**Do not set the arena cap by hand.** The planner measures it: the ceiling is
all free VRAM minus the reserve, and what the model needs is read from the file
on disk to decide whether it goes on the card at all and with which batch. An
earlier version of this guide told you to set `3072` on an 8 GB card, because
the ceiling used to be a 2048 MiB constant applied with `.min()` on every
machine ever built, and `bge-reranker-v2-m3` does not fit in 2048 — so every GPU
install failed with
`BFCArena::AllocateRawInternal: Available memory of N is smaller than requested
bytes of M` until somebody overrode it. That is fixed; a hand-picked number now
only overrides a measured one.

If you set `GPU_MEM_LIMIT_MB` anyway and it lands below what the model needs,
the daemon raises it to the measured floor and logs an `ERROR` naming both
numbers rather than failing at the first search. If you genuinely want the
reranker off the card, the coherent way to say so is `RERANK_DEVICE=cpu` — a
GPU with an impossible ceiling is not a configuration, it is a crash with extra
steps. Verify either way with `memory-industry doctor --deep`, which now really
does load the reranker.

## 4. The token

Generate a long random one. `serve` refuses to bind a routable address with a
token under 32 characters, so `1234` will not get past startup — that check
exists because on a LAN the token is the whole boundary, and nothing slows an
attacker down between attempts.

Never commit it; never put it in the systemd unit or the scheduled task — those
are versioned. It belongs in the environment file that the unit loads.

    # Linux / macOS
    head -c 32 /dev/urandom | base64

    # Windows PowerShell
    [Convert]::ToBase64String((1..32 | % { Get-Random -Max 256 }))

## 5. The address

Bind the **concrete** address of the interface the clients use. It lives in one
place, the environment file `setup service` writes, as `CUBA_HTTP_ADDR`:

    CUBA_HTTP_ADDR=192.168.0.10:8787

The launcher and the systemd unit do not repeat it: `serve` reads it from that
file, so editing the file and running `--apply` again is the whole change.
`setup service --apply` without `--profile` keeps the address the file already
has; it never drops a LAN install back to loopback on its own. Going back to
loopback is explicit: `--profile loopback`.

Not `0.0.0.0:8787`. That binds every interface, including a VPN adapter or a
phone hotspot — interfaces you did not mean to serve.

If the port is already taken, `serve` says so ("is already in use") and exits;
it does not guess another one. Pick a free port and put it in the client config
too. If the address is not assigned to this machine yet — a DHCP lease that has
not arrived at boot — `serve` waits and retries for up to five minutes, then
exits saying so ("is not assigned to this machine"). The wait is
`MEMORY_INDUSTRY_BIND_WAIT_SECS` (`0` tries once). A DHCP lease that gives the
host a different address needs a reservation on the router or a static address
on the host: the daemon waits for the address you named, it does not follow a
new one.

> **Local-environment note, not a product rule.** On a workstation that also
> runs Cursor, `8787` and `8788` may collide with the editor's OAuth callback,
> depending on version. That is a fact about that machine, not about
> MemoryIndustry. If it bites, move the daemon and update the clients.

## 6. Firewall

Allow inbound TCP on the chosen port **from the client addresses only**. Do not
open the whole subnet.

The daemon does what it can on its side: ten wrong bearer tokens from one
address inside a minute and that address has to wait, so a token cannot be
guessed at the speed the daemon answers. A successful call clears the count, so
an editor batching real work is never slowed down. The firewall is still the
first line — this is the second. On Windows this needs an administrator; if the account
installing the daemon is not one, the daemon will run and answer locally while
remote clients time out — that symptom is a missing firewall rule, not a broken
daemon.

`setup service --apply --register` (section 9) opens the rule for you, for the
Private and Domain network profiles and for the chosen port only, and does
nothing for a loopback address. To see what exists:

    netsh advfirewall firewall show rule name=memory-industry

## 7. Each client machine

    {
      "mcpServers": {
        "memory-industry": {
          "type": "http",
          "url": "http://192.168.0.10:8787/mcp",
          "headers": {
            "Authorization": "Bearer <CUBA_HTTP_TOKEN>",
            "Mcp-Client-Id": "workstation-alice"
          }
        }
      }
    }

**`Mcp-Client-Id` should be different on every machine.** It is how the daemon
keeps one machine's open `jornada` and root project from becoming another's.
Use something that cannot collide: the hostname, or the hostname plus the user.

If two machines do send the same id, the daemon now tells them apart by the
address they connected from, so copying one config to every workstation no
longer merges their sessions. You can make that explicit — and survive a DHCP
lease change — with an `Mcp-Machine-Id` header:

      "Mcp-Machine-Id": "workstation-alice"

Callers on the daemon's own machine are always one identity, so a local client
keys exactly as it did before.

## 8. Check it from a client

    curl -s http://192.168.0.10:8787/health

`{"status":"ok"}` means the daemon is up and the database answers. It does not
mean the reranker loaded.

Without the bearer token the answer is deliberately thin: it names the graph
backend and whether it answers, and nothing else. With the token it also
carries the connected clients and the backend's last error — which can name an
internal host and port, and is why it is behind the token. For the models, on
the daemon machine:

    memory-industry doctor --deep

## 9. Keeping it running

Whichever platform, the daemon loads the environment file rather than carrying
the token inline, and `setup service` writes that file once and never rotates
the token behind your back.

Look at the plan first: it writes nothing.

    memory-industry setup service --profile lan --addr 192.168.0.10:8787

### Windows (the host), from an elevated PowerShell

    memory-industry setup service --profile lan --addr 192.168.0.10:8787 --apply --register

That writes three files under `%LOCALAPPDATA%\MemoryIndustry\` (the launcher
`memory-industry.cmd`, the task definition and the environment file, which holds
the token), then registers a scheduled task named `memory-industry` and opens
the firewall rule. Without `--register` it only prints the commands it would
have run. Without an elevated shell, `--register` refuses and says which right
is missing; it never half-installs.

What the task does, and what it implies:

- It starts **at boot**, with nobody logged in, and again at logon; every five
  minutes it asks to start again, and a second copy never runs. If the daemon
  dies it is restarted after a minute, without a practical limit.
- It runs as **SYSTEM**: a service account, so Windows stores no password, and
  it works with the machine sitting at the sign-in screen. The consequence is
  that the daemon runs with all the privileges the machine has. Treat the token
  and the environment file accordingly.
- Because SYSTEM has no profile of its own, every path baked into the task, the
  launcher and the log is **absolute**, and the launcher sets `USERPROFILE` to
  the home of whoever installed before it reads the environment file, so the
  model cache is found. If the `.exe` moves, or the installing user changes,
  run `--apply again` (with `--register`): the paths are written in.
- The launcher appends the daemon's output to `memory-industry.log`, in the same
  folder as the environment file. At the next start, a log over 5 MiB moves to
  `memory-industry.log.1`, replacing the one before. A daemon that refuses to
  start leaves its reason there.
- The database container starts when Docker Desktop does, which is at sign-in,
  not at boot. Until then the daemon answers, but the tools that need the
  database fail; see the table below.

To change the address, edit `CUBA_HTTP_ADDR` in the environment file and run
`--apply --register` again, or pass `--profile lan --addr <new>`. `--apply`
without `--profile` keeps what the file says.

Look at what is installed, read-only:

    schtasks /Query /TN memory-industry /V /FO LIST
    netsh advfirewall firewall show rule name=memory-industry

To remove it, `setup service --uninstall` prints the exact commands and runs none:

    memory-industry setup service --uninstall

On Windows they are `schtasks /Delete /TN memory-industry /F` and
`netsh advfirewall firewall delete rule name=memory-industry`.

### Linux

The unit and socket in `packaging/`, generated by the same command with
`--apply` (there is no task and no firewall rule to register on Linux; open the
port with your own firewall):

    memory-industry setup service --profile lan --addr 192.168.0.10:8787 --apply

The service no longer carries the address on its `ExecStart` line, the
socket does, and for a routable address the socket sets `FreeBind=yes` so it can
open before the address exists.

### When it does not come back

Run `memory-industry doctor` on the host: it reports the address and profile the
environment file holds, whether the task is registered, and the last error line
of the log.

| What you see | Why | What to do |
| --- | --- | --- |
| The log says "is not assigned to this machine" | The address belongs to no interface yet, or the host got another one | Check the address with `ipconfig` (Windows) or `ip addr` (Linux); fix `CUBA_HTTP_ADDR`; give the host a fixed address |
| The log says "is already in use" | Another process holds the port | Find it with `netstat -ano`, or pick another port in both the file and the clients |
| The log says "refusing to bind" | `CUBA_HTTP_TOKEN` is missing or shorter than 32 characters | Fix the environment file; do not copy the token into the task |
| Local answers, remote clients time out | The firewall rule is missing, or the network profile is Public | Run `--apply --register` elevated; check `netsh advfirewall firewall show rule name=memory-industry` |
| The daemon does not start after a reboot | The task is not registered, or it ran and failed | `schtasks /Query /TN memory-industry /V /FO LIST` for the last result; read `memory-industry.log` |
| Search works but only by lexical search | No models were found: under SYSTEM the home is not the user's | Check `USERPROFILE` in the launcher, or set `ONNX_MODEL_PATH` in the environment file |
| The tools fail but `/health` answers; Postgres is not up after a reboot | The container starts at sign-in, not at boot | Wait for Docker Desktop, or start the container from a task of its own |
| `--register` refuses | The shell is not elevated | Open PowerShell as administrator |
