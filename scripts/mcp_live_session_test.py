#!/usr/bin/env python3
"""
Live MCP session test — one long-running cuba-memorys process.

Unlike e2e_all_tools.py (one tools/call per subprocess), this exercises the
real stdin/stdout protocol: initialize → initialized → tools/list → 25× tools/call.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

# Windows consoles often default to cp1252; gate logs must not die on arrows.
if hasattr(sys.stdout, "reconfigure"):
    try:
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")
    except Exception:
        pass

ROOT = Path(__file__).resolve().parent.parent
RUST = ROOT / "rust"
BINARY = Path(
    os.environ.get(
        "CUBA_BINARY_PATH",
        str(RUST / "target" / "release" / "cuba-memorys"),
    )
)
DATABASE_URL = os.environ.get(
    "DATABASE_URL", "postgresql://cuba:memorys2026@127.0.0.1:5488/brain"
)

TOOL_CALLS: List[Tuple[str, str, Dict[str, Any]]] = [
    ("cuba_jornada", "current", {"action": "current"}),
    ("cuba_alma", "get", {"action": "get", "name": "_e2e_cronica"}),
    ("cuba_cronica", "list", {"action": "list", "entity_name": "_e2e_cronica"}),
    ("cuba_faro", "search", {"query": "memory", "mode": "hybrid", "limit": 3}),
    ("cuba_puente", "traverse", {
        "action": "traverse",
        "start_entity": "_e2e_cronica",
        "max_depth": 1,
    }),
    ("cuba_alarma", "report", {
        "error_type": "McpLiveTest",
        "error_message": "live session smoke test",
        "context": {"source": "mcp_live_session_test.py"},
    }),
    ("cuba_expediente", "search", {"query": "McpLiveTest"}),
    ("cuba_decreto", "list", {"action": "list"}),
    ("cuba_vigia", "summary", {}),
    ("cuba_zafra", "stats", {"action": "stats"}),
    ("cuba_reflexion", "analyze", {"action": "analyze"}),
    ("cuba_hipotesis", "explain", {"action": "explain", "effect": "memory"}),
    ("cuba_contradiccion", "scan", {"action": "scan"}),
    ("cuba_centinela", "list", {"action": "list"}),
    ("cuba_calibrar", "stats", {"action": "stats"}),
    ("cuba_ingesta", "parse", {
        "action": "parse",
        "entity_name": "_e2e_cronica",
        "text": "Live MCP session test paragraph.",
    }),
    ("cuba_proyecto", "list", {"action": "list"}),
    ("cuba_pre_compact", "restore", {"action": "restore"}),
    ("cuba_sync", "status", {"action": "status"}),
    ("cuba_juez", "scan_entity", {
        "action": "scan_entity",
        "entity_name": "_e2e_cronica",
        "max_pairs": 1,
    }),
    ("cuba_pizarra", "read", {"action": "read"}),
    ("cuba_archivo", "tail", {"action": "tail", "limit": 2}),
    ("cuba_whoami", "identity", {}),
    ("cuba_artefacto", "list", {"action": "list", "limit": 5}),
    ("cuba_contexto", "budget", {"budget_chars": 4000}),
    ("cuba_receta", "list", {"action": "list", "limit": 5}),
    ("cuba_tools", "list", {}),
]

SKIP_LIVE: set[str] = set()  # soft-skips forbidden; every live tool must run

class McpSession:
    def __init__(self) -> None:
        env = os.environ.copy()
        env["DATABASE_URL"] = DATABASE_URL
        env["CUBA_JUDGE"] = os.environ.get("CUBA_JUDGE", "heuristic")
        self.proc = subprocess.Popen(
            [str(BINARY)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )
        self._responses: List[Dict[str, Any]] = []
        self._stderr_lines: List[str] = []
        self._lock = threading.Lock()
        self._reader = threading.Thread(target=self._read_stdout, daemon=True)
        self._reader.start()
        # Without a reader, a verbose ORT/GPU binary fills the pipe and deadlocks
        # mid-suite (seen after the first few tools/call on Windows).
        self._err_reader = threading.Thread(target=self._read_stderr, daemon=True)
        self._err_reader.start()

    def _read_stdout(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                msg = json.loads(line)
            except json.JSONDecodeError:
                continue
            with self._lock:
                self._responses.append(msg)

    def _read_stderr(self) -> None:
        assert self.proc.stderr is not None
        for line in self.proc.stderr:
            with self._lock:
                if len(self._stderr_lines) < 200:
                    self._stderr_lines.append(line.rstrip())

    def request(
        self, method: str, params: Optional[Dict[str, Any]] = None, timeout: float = 30.0
    ) -> Dict[str, Any]:
        req_id = str(uuid.uuid4())
        payload = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            payload["params"] = params
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()
        deadline = time.time() + timeout
        while time.time() < deadline:
            with self._lock:
                for i, msg in enumerate(self._responses):
                    mid = msg.get("id")
                    if mid == req_id or str(mid) == str(req_id):
                        return self._responses.pop(i)
            time.sleep(0.05)
        raise TimeoutError(
            f"no response for {method} id={req_id}"
            + (
                f"; stderr_tail={self._stderr_lines[-8:]!r}"
                if self._stderr_lines
                else ""
            )
        )
    def notify(self, method: str, params: Optional[Dict[str, Any]] = None) -> None:
        payload: Dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            payload["params"] = params
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()

    def close(self) -> None:
        if self.proc.stdin:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()

def parse_tool_result(response: Dict[str, Any]) -> Tuple[bool, str]:
    if "error" in response:
        err = response["error"]
        return False, f"JSON-RPC error: {err}"
    result = response.get("result", {})
    if result.get("isError"):
        content = result.get("content", [])
        text = content[0].get("text", "") if content else ""
        return False, f"tool isError: {text[:200]}"
    content = result.get("content", [])
    if not content:
        return False, "empty content"
    text = content[0].get("text", "")
    try:
        body = json.loads(text)
    except json.JSONDecodeError:
        return True, "non-json text ok"
    if isinstance(body, dict) and "error" in body:
        return False, f"tool body error: {body['error']}"
    return True, "ok"

def main() -> int:
    if not BINARY.is_file():
        print(f"error: binary not found: {BINARY}", file=sys.stderr)
        print("run: cd rust && cargo build --release", file=sys.stderr)
        return 1

    print("=" * 60)
    print("MCP LIVE SESSION TEST (single process)")
    print("=" * 60)
    print(f"Binary: {BINARY}")
    print(f"DB:     {DATABASE_URL}")

    session = McpSession()
    failed: List[str] = []
    passed = 0

    try:
        init = session.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "mcp-live-test", "version": "1.0"},
            },
        )
        if "error" in init:
            print(f"FAIL initialize: {init['error']}")
            return 1
        srv = init.get("result", {}).get("serverInfo", {})
        print(f"PASS initialize → {srv.get('name', '?')} v{srv.get('version', '?')}")

        session.notify("notifications/initialized")

        tools_resp = session.request("tools/list")
        if "error" in tools_resp:
            print(f"FAIL tools/list: {tools_resp['error']}")
            return 1
        tools = tools_resp.get("result", {}).get("tools", [])
        names = {t.get("name") for t in tools}
        print(f"PASS tools/list → {len(tools)} tools")
        expected = {t[0] for t in TOOL_CALLS} | SKIP_LIVE
        missing = expected - names
        if missing:
            print(f"FAIL tools/list missing: {sorted(missing)}")
            return 1

        error_id: Optional[str] = None
        live_entity = "_mcp_live_test"

        seed = session.request(
            "tools/call",
            {
                "name": "cuba_alma",
                "arguments": {
                    "action": "create",
                    "name": live_entity,
                    "entity_type": "concept",
                },
            },
        )
        ok, detail = parse_tool_result(seed)
        if ok:
            print(f"PASS cuba_alma/create (seed {live_entity})")
            passed += 1
        else:
            print(f"WARN cuba_alma/create seed: {detail}")

        for tool_name, label, args in TOOL_CALLS:
            if tool_name in ("cuba_alma", "cuba_cronica", "cuba_puente", "cuba_hipotesis", "cuba_juez", "cuba_ingesta"):
                if tool_name == "cuba_alma" and label == "get":
                    args = {"action": "get", "name": live_entity}
                elif tool_name == "cuba_cronica":
                    args["entity_name"] = live_entity
                elif tool_name == "cuba_puente":
                    args["start_entity"] = live_entity
                elif tool_name == "cuba_hipotesis":
                    args["effect"] = live_entity
                elif tool_name == "cuba_juez":
                    args["entity_name"] = live_entity
                elif tool_name == "cuba_ingesta":
                    args["entity_name"] = live_entity
            if tool_name in SKIP_LIVE:
                print(f"SKIP {tool_name} (see e2e_all_tools.py)")
                continue

            resp = session.request(
                "tools/call",
                {"name": tool_name, "arguments": args},
                timeout=45.0,
            )
            ok, detail = parse_tool_result(resp)
            if ok:
                passed += 1
                print(f"PASS {tool_name}/{label}")
                if tool_name == "cuba_expediente" and "result" in resp:
                    try:
                        text = resp["result"]["content"][0]["text"]
                        body = json.loads(text)
                        results = body.get("results", [])
                        if results:
                            error_id = results[0].get("id")
                    except (KeyError, IndexError, json.JSONDecodeError):
                        pass
            else:
                failed.append(f"{tool_name}/{label}: {detail}")
                print(f"FAIL {tool_name}/{label}: {detail}")

        for skip_tool in sorted(SKIP_LIVE):
            print(f"NOTE {skip_tool}: full path in rust/tests/e2e_all_tools.py")

        if error_id:
            resp = session.request(
                "tools/call",
                {
                    "name": "cuba_remedio",
                    "arguments": {
                        "error_id": error_id,
                        "solution": "resolved in live session test",
                    },
                },
            )
            ok, detail = parse_tool_result(resp)
            if ok:
                passed += 1
                print("PASS cuba_remedio/resolve")
            else:
                failed.append(f"cuba_remedio/resolve: {detail}")
                print(f"FAIL cuba_remedio/resolve: {detail}")

        add_obs = session.request(
            "tools/call",
            {
                "name": "cuba_cronica",
                "arguments": {
                    "action": "add",
                    "entity_name": live_entity,
                    "content": f"Live session eco probe {uuid.uuid4()}",
                    "observation_type": "fact",
                },
            },
            timeout=45.0,
        )
        ok, detail = parse_tool_result(add_obs)
        obs_id = None
        if ok:
            try:
                body = json.loads(add_obs["result"]["content"][0]["text"])
                obs_id = (
                    body.get("id")
                    or body.get("observation_id")
                    or body.get("reinforced_id")
                )
            except (KeyError, IndexError, json.JSONDecodeError, TypeError):
                pass
        if obs_id:
            eco = session.request(
                "tools/call",
                {
                    "name": "cuba_eco",
                    "arguments": {
                        "action": "positive",
                        "entity_name": live_entity,
                        "observation_id": str(obs_id),
                    },
                },
            )
            ok, detail = parse_tool_result(eco)
            if ok:
                passed += 1
                print("PASS cuba_eco/positive")
            else:
                failed.append(f"cuba_eco/positive: {detail}")
                print(f"FAIL cuba_eco/positive: {detail}")
        else:
            failed.append(f"cuba_eco/positive: no observation_id ({detail})")
            print(f"FAIL cuba_eco/positive: no observation_id ({detail})")

    finally:
        session.close()

    print("\n" + "=" * 60)
    print(f"Passed: {passed}  Failed: {len(failed)}")
    if failed:
        print("Failures:")
        for f in failed:
            print(f"  - {f}")
        return 1
    print("All live MCP tool calls OK in single session.")
    return 0

if __name__ == "__main__":
    sys.exit(main())