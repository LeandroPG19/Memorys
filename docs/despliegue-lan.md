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
`CUBA_NLI_DEVICE=cpu`. If you force the embedder or the NLI onto the GPU anyway,
its session runs with no arena ceiling of ours: the plan weighs only the
reranker, and `SameAsRequested` grows only by what the session asks for
(decided in 0.30).

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
new one. Section 10 shows how to give the host a fixed address from PowerShell
when the router is not yours to change.

> **Local-environment note, not a product rule.** On a workstation that also
> runs Cursor, `8787` and `8788` may collide with the editor's OAuth callback,
> depending on version. That is a fact about that machine, not about
> MemoryIndustry. If it bites, move the daemon and update the clients.

### Automatic address and discovery (0.29)

Since 0.29 the host can pick its address itself. `setup service` with the LAN
profile and **without** `--addr` writes

    CUBA_HTTP_ADDR=lan:8787

and from then on the daemon listens on `127.0.0.1:8787` and on the private address
(10/8, 172.16/12, 192.168/16) the host uses to reach its default route, whatever it
is today. Every 10 seconds it looks again; when the DHCP hands the host a new
address it opens the new one, closes the old one and logs `lan address changed`.
It never listens on `0.0.0.0`. `/health` says where it is now, in
`"lan": {"mode", "address", "since"}`, and `doctor` prints the same. An explicit
`--addr 192.168.0.10:8787` keeps the fixed behaviour described above.

    memory-industry setup service --profile lan --apply --register

On Windows `--register` opens two firewall rules on the same port, both for the
private and domain profiles only: TCP for the service and UDP for the
**announcement**. A client that asks on the LAN (a broadcast to UDP 8787) gets back
the host's identity and port, and only from private addresses, with a rate limit.
The answer is just a candidate: the client still asks for the proof of identity
before it sends anything, exactly as with a saved address or a name.

A client looks for its host in this order: the last address that worked, the saved
ones, the UDP announcement, the host's name, and a sweep of its own /24. If the
company network drops broadcasts, the name and the sweep still work; if the host is
on another subnet, type its name or address once (the bridge's window offers it),
and there a fixed address on the host (section 10) is the sensible choice.

The Windows visit packages do all of this without commands:

- On the host, `INSTALAR-EQUIPO-REAL.cmd` installs in automatic mode (it shows the
  detected address but does not ask for one) and registers the per-user task
  `MemoryIndustry Madre aviso`, which tells whoever signs in that the host is up and
  at which address, and offers to fix the address when it changed.
  `GENERAR-CODIGO.cmd` runs `account pair` for a person and shows the code, the
  host's name and its address.
- On each client, `memory-industry-0.29-CLIENTE.zip` holds `CONECTAR-A-LA-MADRE.cmd`:
  it copies the binary to `%LOCALAPPDATA%\MemoryIndustry\bin`, asks for the code,
  pairs (asking for the host's name or address only if it is not found), installs
  the bridge and configures Claude Code and Cursor when present. No administrator.

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

This section is the shared-token route. For Windows clients there is a route
with no token and no address in the client's config, and with a credential you
can revoke per device: section 11.

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

Run `memory-industry doctor` with **the same account that ran `setup service`**.
The three files live under that account's `%LOCALAPPDATA%`, and `doctor` reads
them from the `%LOCALAPPDATA%` of whoever runs it: if IT installed the service
with its own administrator account and an operator runs `doctor` from theirs, it
says there is no service install under the operator's profile — the service is
running, `doctor` is looking in another user's folder.

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

## 10. Fixed address on the host

Clients find the host again by the address they saved, by its name, and by
sweeping their own subnet, and they only believe an answer that proves it is the
host. A host whose address keeps changing still works, but every change costs
the clients a search. A fixed address avoids that, and it is the only step in
this guide that changes the host's network settings. Do it once, from an
**elevated** PowerShell (run as administrator).

First look at what the host has now, and write down the `InterfaceIndex`, the
address, the gateway and the DNS servers of the interface the clients reach:

    ipconfig /all
    Get-NetIPConfiguration

Then choose the address. **Choose one outside the DHCP range** of the router:
the router hands out addresses from a pool, and an address inside it can be given
to another machine the day the host is switched off. Windows cannot see that
range, because the router is not reachable from the host. Ask whoever administers
the router for the pool (or ask them to reserve the address you picked for the
host), and choose an address on the same subnet as the gateway that falls outside
it. The examples use `192.168.0.10`, with a pool that starts higher up, and
interface 12:

    $ix = 12
    Set-NetIPInterface -InterfaceIndex $ix -Dhcp Disabled
    New-NetIPAddress -InterfaceIndex $ix -IPAddress 192.168.0.10 -PrefixLength 24 -DefaultGateway 192.168.0.1
    Set-DnsClientServerAddress -InterfaceIndex $ix -ServerAddresses 192.168.0.1

Use the values you wrote down: the prefix length that matches the subnet mask,
the gateway of that interface, and its DNS servers (the gateway itself is the
common answer on a small office network). If `New-NetIPAddress` answers that the
address or the default route already exists, the lease the host had is still
configured: remove it (`Remove-NetIPAddress` for the old address, `Remove-NetRoute`
for the old default route, both with `-InterfaceIndex $ix`) and run the command
again. This sequence has not been run on a real host yet; the changelog lists it
as not verified.

Check it with `ipconfig /all`: the interface must show the address and say that
DHCP is disabled. To go back to DHCP:

    Set-NetIPInterface -InterfaceIndex $ix -Dhcp Enabled
    Set-DnsClientServerAddress -InterfaceIndex $ix -ResetServerAddresses

Then put the address in `CUBA_HTTP_ADDR` (section 5) and run `--apply --register`
again (section 9).

The check that matters comes from a client, once the clients are paired
(section 11):

    memory-industry bridge doctor

If another machine ever answers on the host's address, the bridge does not send
it the credential: `bridge doctor` lists that address as unable to prove it is the
host and exits 1. That is the symptom of an address inside the DHCP range.

## 11. Accounts and the local bridge

The shared token (sections 4 and 7) is one secret for everybody: it cannot be
taken away from one lost PC without changing it on every other, and nothing says
who wrote what. Accounts replace it with one credential per device.

What an account is, and is not. It authenticates a device, it attributes what the
device writes (`cuba_whoami` and the logs show the account and the device), and it
can be revoked at once. It does **not** partition the graph and it is **not** a
permission: every account can read everything and call every tool, `cuba_forget`
and the `admin/*` routes included, exactly as the shared token can. Hand out
accounts to people you would hand the token to.

### On the host: create the account and issue a code

These run on the host and talk to the database directly; they need neither the
daemon nor its token.

    memory-industry account add ana
    memory-industry account pair ana --device pc-02
    memory-industry account list

`account pair` prints a pairing code such as `K7QM-3WXP-9HNR` once, together with
the host fingerprint. The code is valid for 24 hours, works once, and is stored
only as a key, so it cannot be shown again: issue another if it is lost. Give the
code to the person at the client PC.

### On a client: pair in under two minutes, without administrator

A Windows PC pairs in about two minutes, and every step works without administrator
rights, as an ordinary user:

1. Put `memory-industry.exe` in a folder you own, for example
   `%LOCALAPPDATA%\MemoryIndustry\`.
2. Pair with the code. The code is offered only to machines on this PC's subnet
   that answer as MemoryIndustry, and the host must prove it knows the code before
   anything is saved. If the host is on another subnet, name it with
   `--host madre` or `--host 192.168.0.10`.

        memory-industry bridge pair K7QM-3WXP-9HNR

   It prints the host fingerprint; it must be the same one `account pair` showed.
3. Start the bridge at every sign-in of this user:

        memory-industry bridge install

   That creates a per-user task named `MemoryIndustry Bridge` (no administrator, no
   window) and starts it now. `bridge install --print` shows the commands without
   running them; `bridge uninstall` removes the task.
4. Point the MCP client at the bridge. `all` writes Claude Code, Claude's
   `.mcp.json`, Cursor and Warp; name one to write only that client. Without
   `--apply` it only shows the plan; with it, it copies the file first, leaves every
   other server alone, and does nothing on a second run:

        memory-industry setup client claude --client-id pc-02 --apply
        memory-industry setup client cursor --client-id pc-02 --apply

   Restart the client. `--client-id` should be different on every PC, as in
   section 7.

Check it:

    memory-industry bridge status
    memory-industry bridge doctor

### What the bridge is

A small local process, `memory-industry bridge run`, started by that task. It
listens on `127.0.0.1:8797` and nowhere else, and it **holds the credential**:
the client config only says `http://127.0.0.1:8797/mcp`, so it carries no token
and no address of the host, and nothing in it has to change when the host's
address does. Each request goes to the host with the credential added. The
bridge finds the host again by trying, in order, the last address that worked, the
saved ones, the host's name, and the other addresses of this PC's /24; it sends the
credential only to an answer that proves it is the host, and it refuses a web page
that tries to use it (a foreign `Origin` or `Host`).

Its state is one file, `%LOCALAPPDATA%\MemoryIndustry\bridge.json`, with the
host's identity, this device's credential and the addresses seen. Only your
Windows user can open it (the access list is set before the secret is written), but
administrators and SYSTEM can read any file of yours. The port belongs to the PC,
not to the user: one PC is one user's bridge.

### Revoking

On the host:

    memory-industry account revoke ana
    memory-industry account revoke-device ana pc-02

The first closes every device of the account, the second only the devices of that
account with that label. It takes effect on the next request, with no restart and
nothing to change on the client: the bridge answers «credencial revocada» and
stops searching until the operator issues a new code. A lost PC is
`account revoke-device`; rotating a credential is revoke and pair again.

### Closing the shared token

When every client is paired, add `MEMORY_INDUSTRY_REQUIRE_ACCOUNTS=1` to the
environment file and restart the daemon. On a non-loopback address the shared
token then stops working on `/mcp` and `/events` and answers 401 with a message
that says how to pair; the peer token keeps working for sync, and `/health` still
takes the shared token for monitors. On a loopback address the setting is ignored
with a warning. `memory-industry doctor` fails if it is on and no credential is
active.

### Reading `bridge doctor`

It tries the host the way the bridge does and prints one line per candidate: where
the address came from (`indicada`, `última buena`, `guardada`, `por nombre`,
`barrido`), the address, and what it answered. The last line is the host it
found and its fingerprint, `huella`, which must match the one `account pair`
printed. It exits 0 when a host proved itself and 1 when none did, and then the
lines say what was tried and what to do. A sweep prints the answers that matter
and a count of the silent addresses it left out.

    fijada: madre (huella 5F3A-91C2-77D0-AB14), cuenta ana/pc-02
    candidatos probados: 3
    última buena 192.168.0.20:8787  sin respuesta (conexión rechazada o sin ruta)
    guardada     192.168.0.30:8787  otra instalación (huella 91AA-0B7E-33C4-D5E8)
    barrido      192.168.0.10:8787  verificada (5F3A-91C2-77D0-AB14)
    madre: 192.168.0.10:8787 (huella 5F3A-91C2-77D0-AB14)

The report never contains the credential.

On a Windows client the word for a host that does not answer can read `tiempo agotado` where the example above shows `conexión rechazada o sin ruta`: Windows refuses a connection to a port where nobody listens after about two seconds, and the bridge gives up at two. It is the same fact — that address did not answer — and either word means the same next step (is the host up, is the firewall rule there, did its address change). It was measured on loopback only; the remote case is scenario M2.

### What the proof of identity does and does not cover

Traffic is still HTTP in clear text: TLS is the next version. What the proof does
now is stop a client from handing its credential to the wrong machine: a host
name that resolves to someone else, a PC that inherited the host's old address
from DHCP, a sweep that bumps into another server. Pairing proves the same thing
in the other direction: a machine that does not know the code cannot complete it.

It does not cover an attacker who sits on the path between a client and the host
(ARP spoofing), who can relay the proof to the real host and keep the credential;
and anybody who can read the traffic reads the credential as they read the shared
token today. What you gain already is that each device has its own credential,
revocable at once, which the shared token is not.

### When it does not work

| What you see | Why | What to do |
| --- | --- | --- |
| `bridge pair` says «la madre no aceptó el código» | The code is mistyped, older than 24 hours, or already used | Ask the operator for a new `account pair`; the code never uses 0, O, 1 or I |
| `bridge doctor` or the client says «no se encontró la madre» | The host is off or on another network, a firewall blocks the port, or the host is outside this PC's /24 | Check the host with section 8; pair again with `--host <name or address>` |
| The error says «la madre cambió de huella» | Another MemoryIndustry installation answered where the host used to be | If the host was reinstalled, pair again with a new code; if not, another machine took the address (section 10) |
| The bridge says «credencial revocada» | The operator revoked this device or its account | Ask for a new code and run `bridge pair` again |
| `bridge run` says «cannot listen on 127.0.0.1» | Another bridge, or another program, holds the port | `bridge status` shows the port; close the other one, or use `--port` in `bridge run` and in `setup client` |
