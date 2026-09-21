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

> **Known rough edge — the arena cap.** `CUBA_GPU_MEM_LIMIT_MB` defaults to
> `2048`, and the resource planner never raises it above that number on any
> machine. `bge-reranker-v2-m3` does not fit in 2048 MiB, so on a real card the
> session fails to open with
> `BFCArena::AllocateRawInternal: Available memory of N is smaller than
> requested bytes of M`, and the daemon silently falls back to returning the
> ranking unreordered. Until that is fixed, **set the cap yourself**: free VRAM
> minus roughly 512 MiB of headroom. On an 8 GB card, `3072` is enough; more is
> fine if the card is idle. Verify with `memory-industry doctor --deep`.

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

Bind the **concrete** address of the interface the clients use:

    CUBA_HTTP_ADDR=192.168.0.10:8787

Not `0.0.0.0:8787`. That binds every interface, including a VPN adapter or a
phone hotspot — interfaces you did not mean to serve.

If the port is already taken, `serve` says so and exits; it does not guess
another one. Pick a free port and put it in the client config too.

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

Linux: the unit and socket in `packaging/`. Windows: a scheduled task that runs
at logon and restarts on failure. Whichever you use, it must load the
environment file rather than carrying the token inline.
