#!/usr/bin/env python3
"""
Sembrado de la base desechable de la eval smoke, por MCP.

Por qué existe (D-J3a): la eval puntúa contra un corpus fijo, no contra el vivo.
El corpus vivo es la memoria personal de Leandro (datos personales) y no entra
en el repo, así que el contenido de la eval es rust/eval-datasets/smoke-corpus.jsonl,
sintético y versionado. Se mete por el propio binario, por MCP en una sola sesión
stdio, para que cada observación pase por el mismo camino que una real (esquema,
embeddings, dedup). Un pg_dump acoplaría el fixture a la versión del esquema.

Ámbito (riesgo 6): la siembra abre una sesión con `cuba_jornada start project=eval-smoke`,
así que cada observación lleva project_id de ese proyecto. La eval no tiene sesión
(`eval/harness.rs` llama a `handlers::faro::handle` fuera de cualquier ámbito), y
`project::current_project_id` devuelve None; sin proyecto activo la búsqueda no filtra,
así que ve lo sembrado. Si la eval llegara a tener un proyecto activo, tendría que ser eval-smoke.

Seguridad: la URL y su clave nunca se imprimen (los errores se limpian con _scrub).
La base tiene que llamarse brain_gate* (la desechable del juez); cualquier otro nombre sale 2.

Uso:
  DATABASE_URL=URL python3 -I scripts/eval_seed.py --binary RUTA [--fixture RUTA]
  python3 -I scripts/eval_seed.py --check-fixture [--fixture RUTA] [--queries RUTA]

Salida de la siembra: una línea `seeded N observations` en stdout y exit 0.
Exit 2: uso o configuración (falta URL, base que no es brain_gate*, fixture ausente).
Exit 1: error de MCP o fixture que no cumple J3.3.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import subprocess
import sys
import threading
from pathlib import Path
from typing import Any, Dict, List, Optional
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_FIXTURE = ROOT / "rust" / "eval-datasets" / "smoke-corpus.jsonl"
DEFAULT_QUERIES = ROOT / "rust" / "eval-datasets" / "smoke.jsonl"

EVAL_PROJECT = "eval-smoke"
DB_PREFIX = "brain_gate"
MIN_NON_RELEVANT = 10
OBSERVATION_TYPES = {
    "fact", "decision", "lesson", "preference", "context",
    "tool_usage", "error", "solution",
}
SOURCES = {"agent", "user", "error_detection", "consolidation", "inference"}
REQUIRED_KEYS = ("entity_name", "observation_type", "content")
NEEDS_NO_REAL_DATA = ("@", "http", "://")


class SeedError(Exception):
    """Fallo de MCP o de la siembra. Su texto ya viene sin URL ni clave."""


def load_jsonl(path: Path) -> List[Dict[str, Any]]:
    rows: List[Dict[str, Any]] = []
    for number, raw in enumerate(path.read_text(encoding="utf-8").splitlines(), start=1):
        if not raw.strip():
            continue
        try:
            rows.append(json.loads(raw))
        except json.JSONDecodeError as exc:
            raise ValueError(f"{path.name}:{number} no es JSON: {exc.msg}") from exc
    return rows


def validate_observation(row: Dict[str, Any], number: int) -> None:
    for key in REQUIRED_KEYS:
        if not isinstance(row.get(key), str) or not row[key].strip():
            raise ValueError(f"fixture line {number}: falta «{key}» o está vacío")
    if row["observation_type"] not in OBSERVATION_TYPES:
        raise ValueError(f"fixture line {number}: observation_type «{row['observation_type']}» no existe")
    source = row.get("source", "agent")
    if source not in SOURCES:
        raise ValueError(f"fixture line {number}: source «{source}» no existe")


def check_fixture(fixture: Path, queries_path: Path) -> int:
    queries = load_jsonl(queries_path)
    observations = load_jsonl(fixture)
    failures: List[str] = []

    for number, row in enumerate(observations, start=1):
        try:
            validate_observation(row, number)
        except ValueError as exc:
            failures.append(str(exc))
        text = json.dumps(row, ensure_ascii=False)
        for token in NEEDS_NO_REAL_DATA:
            if token in text:
                failures.append(f"fixture line {number}: contiene «{token}», que no va en un fixture sintético")

    lowered = [row["content"].lower() for row in observations if isinstance(row.get("content"), str)]
    satisfied = [False] * len(lowered)

    for index, query in enumerate(queries, start=1):
        markers = [m.lower() for m in query.get("relevant_markers", []) if m]
        hits = 0
        for pos, content in enumerate(lowered):
            if any(marker in content for marker in markers):
                hits += 1
                satisfied[pos] = True
        print(f"consulta {index}: {hits} observaciones relevantes  «{query.get('query', '')}»")
        if hits == 0:
            failures.append(f"consulta {index} no tiene ninguna observación relevante en el fixture")

    non_relevant = satisfied.count(False)
    print(f"observaciones que no satisfacen ninguna consulta: {non_relevant} (mínimo {MIN_NON_RELEVANT})")
    if non_relevant < MIN_NON_RELEVANT:
        failures.append(
            f"solo {non_relevant} observaciones no relevantes; hacen falta al menos {MIN_NON_RELEVANT} para que el orden importe"
        )
    print(f"observaciones en el fixture: {len(observations)}")

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}")
        return 1
    print("OK  fixture sintético cumple J3.3 (sin datos reales por inspección: ver la cabecera)")
    return 0


def database_name(database_url: str) -> str:
    try:
        return urlsplit(database_url).path.lstrip("/")
    except ValueError:
        return ""


def _scrub(text: str, database_url: str) -> str:
    scrubbed = text.replace(database_url, "<url redactada>")
    password = urlsplit(database_url).password
    if password:
        scrubbed = scrubbed.replace(password, "<clave redactada>")
    return scrubbed


class McpStdio:
    def __init__(self, binary: Path, database_url: str) -> None:
        env = os.environ.copy()
        env["DATABASE_URL"] = database_url
        self.database_url = database_url
        self.proc = subprocess.Popen(
            [str(binary)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            env=env,
        )
        self._messages: "queue.Queue[Dict[str, Any]]" = queue.Queue()
        self._stderr: List[str] = []
        self._next_id = 0
        threading.Thread(target=self._read_stdout, daemon=True).start()
        # A binary that fills its stderr pipe would block mid-session.
        threading.Thread(target=self._read_stderr, daemon=True).start()

    def _read_stdout(self) -> None:
        assert self.proc.stdout is not None
        for line in self.proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                self._messages.put(json.loads(line))
            except json.JSONDecodeError:
                continue

    def _read_stderr(self) -> None:
        assert self.proc.stderr is not None
        for line in self.proc.stderr:
            if len(self._stderr) < 50:
                self._stderr.append(line.rstrip())

    def _send(self, payload: Dict[str, Any]) -> None:
        assert self.proc.stdin is not None
        self.proc.stdin.write(json.dumps(payload) + "\n")
        self.proc.stdin.flush()

    def request(self, method: str, params: Optional[Dict[str, Any]] = None, timeout: float = 30.0) -> Dict[str, Any]:
        self._next_id += 1
        req_id = self._next_id
        payload: Dict[str, Any] = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            payload["params"] = params
        self._send(payload)
        try:
            while True:
                msg = self._messages.get(timeout=timeout)
                if msg.get("id") == req_id:
                    return msg
        except queue.Empty as exc:
            tail = " | ".join(self._stderr[-5:])
            detail = f"; stderr: {_scrub(tail, self.database_url)}" if tail else ""
            raise SeedError(f"sin respuesta a {method} en {timeout:.0f} s{detail}") from exc

    def notify(self, method: str) -> None:
        self._send({"jsonrpc": "2.0", "method": method})

    def call_tool(self, name: str, arguments: Dict[str, Any], timeout: float = 60.0) -> Dict[str, Any]:
        response = self.request("tools/call", {"name": name, "arguments": arguments}, timeout=timeout)
        if "error" in response:
            raise SeedError(f"{name}: error JSON-RPC {response['error']}")
        result = response.get("result", {})
        content = result.get("content", [])
        text = content[0].get("text", "") if content else ""
        if result.get("isError"):
            raise SeedError(f"{name}: isError {_scrub(text[:200], self.database_url)}")
        try:
            body = json.loads(text)
        except json.JSONDecodeError as exc:
            raise SeedError(f"{name}: respuesta que no es JSON") from exc
        if isinstance(body, dict) and "error" in body:
            raise SeedError(f"{name}: {_scrub(str(body['error'])[:200], self.database_url)}")
        return body

    def close(self) -> None:
        if self.proc.stdin:
            self.proc.stdin.close()
        try:
            self.proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def seed(binary: Path, database_url: str, fixture: Path) -> int:
    rows = load_jsonl(fixture)
    for number, row in enumerate(rows, start=1):
        validate_observation(row, number)

    session = McpStdio(binary, database_url)
    saved = 0
    try:
        session.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "eval-seed", "version": "1.0"},
            },
        )
        session.notify("notifications/initialized")
        session.call_tool(
            "cuba_jornada",
            {"action": "start", "name": "eval-smoke-seed", "project": EVAL_PROJECT},
        )
        for number, row in enumerate(rows, start=1):
            body = session.call_tool(
                "cuba_cronica",
                {
                    "action": "add",
                    "entity_name": row["entity_name"],
                    "content": row["content"],
                    "observation_type": row["observation_type"],
                    "source": row.get("source", "agent"),
                },
            )
            # Dedup and reinforcement answer ok without inserting a row; counting
            # them would let the eval read fewer observations than the fixture has.
            if body.get("deduplicated") or "reinforced_id" in body:
                raise SeedError(f"fixture line {number}: no se insertó (dedup o refuerzo de otra fila)")
            saved += 1
    finally:
        session.close()

    print(f"seeded {saved} observations")
    if saved != len(rows):
        print(f"FAIL: se guardaron {saved} de {len(rows)} líneas del fixture", file=sys.stderr)
        return 1
    return 0


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Siembra la base de la eval smoke por MCP.")
    parser.add_argument("--binary", type=Path, help="binario de MemoryIndustry (cuba-memorys)")
    parser.add_argument("--fixture", type=Path, default=DEFAULT_FIXTURE)
    parser.add_argument("--queries", type=Path, default=DEFAULT_QUERIES)
    parser.add_argument("--check-fixture", action="store_true", help="solo comprueba J3.3, sin base")
    args = parser.parse_args(argv)

    if args.check_fixture:
        if not args.fixture.is_file():
            print(f"FAIL: falta {args.fixture}")
            return 2
        if not args.queries.is_file():
            print(f"FAIL: falta {args.queries}")
            return 2
        return check_fixture(args.fixture, args.queries)

    url = os.environ.get("DATABASE_URL", "").strip()
    if not url:
        print("FAIL: DATABASE_URL es obligatorio y no puede estar vacío (ver docs/gate.md)", file=sys.stderr)
        return 2
    name = database_name(url)
    if not name.startswith(DB_PREFIX):
        print(
            f"FAIL: la base de la siembra tiene que llamarse {DB_PREFIX}* (la desechable del juez); "
            "el nombre dado no lo cumple",
            file=sys.stderr,
        )
        return 2
    if args.binary is None or not args.binary.is_file():
        print(f"FAIL: falta el binario {args.binary}", file=sys.stderr)
        return 2
    if not args.fixture.is_file():
        print(f"FAIL: falta {args.fixture}", file=sys.stderr)
        return 2

    try:
        return seed(args.binary, url, args.fixture)
    except (SeedError, ValueError, OSError) as exc:
        print(f"FAIL: {_scrub(str(exc), url)}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
