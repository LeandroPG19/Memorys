#!/usr/bin/env python3
"""Realign _sqlx_migrations.checksum with on-disk *.up.sql (SHA-384).

Needed when line endings change (CRLF→LF via .gitattributes) without changing SQL
semantics. Refuses to update a version whose LF and CRLF hashes both disagree
with the DB (that is a content change — restore the original file instead).
"""
from __future__ import annotations

import argparse
import hashlib
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MIG = ROOT / "rust" / "migrations"


def sha384(data: bytes) -> str:
    return hashlib.sha384(data).hexdigest()


def parse_args() -> argparse.Namespace:
    ap = argparse.ArgumentParser()
    ap.add_argument("--container", default="cuba-memorys-db")
    ap.add_argument("--db", default="brain")
    ap.add_argument("--user", default="cuba")
    ap.add_argument("--apply", action="store_true")
    return ap.parse_args()


def docker_psql(args: argparse.Namespace, *psql_flags: str) -> list[str]:
    return [
        "docker",
        "exec",
        args.container,
        "psql",
        "-U",
        args.user,
        "-d",
        args.db,
        *psql_flags,
    ]


def read_db_checksums(args: argparse.Namespace) -> dict[int, str]:
    out = subprocess.check_output(
        docker_psql(
            args,
            "-t",
            "-A",
            "-F",
            "|",
            "-c",
            "SELECT version, encode(checksum,'hex') FROM _sqlx_migrations ORDER BY version",
        ),
        text=True,
    )
    db = {}
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        ver_s, hx = line.split("|", 1)
        db[int(ver_s)] = hx
    return db


def classify_version(ver: int, hx: str):
    """Returns ("update", entry), ("diff", entry) or None when nothing to do."""
    ups = list(MIG.glob(f"{ver:04d}_*.up.sql"))
    if not ups:
        print(f"WARN: version {ver} in DB but no file", file=sys.stderr)
        return None
    data = ups[0].read_bytes()
    lf = sha384(data.replace(b"\r\n", b"\n"))
    crlf = sha384(data.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))
    as_is = sha384(data)
    if hx == as_is:
        return None
    if hx in (lf, crlf) or hx == as_is:
        # line-ending only (or already matching after normalize)
        return ("update", (ver, as_is, ups[0].name))
    return ("diff", (ver, ups[0].name))


def classify(db: dict[int, str]) -> tuple[list, list]:
    updates = []
    content_diffs = []
    for ver, hx in sorted(db.items()):
        verdict = classify_version(ver, hx)
        if verdict is None:
            continue
        kind, entry = verdict
        (updates if kind == "update" else content_diffs).append(entry)
    return updates, content_diffs


def summarize(updates: list, content_diffs: list) -> int | None:
    """Prints the counts; returns an exit code when there is nothing to apply."""
    print(f"line-ending drift: {len(updates)}")
    print(f"content diffs (refused): {len(content_diffs)}")
    for ver, name in content_diffs[:20]:
        print(f"  CONTENT {ver} {name}")
    if content_diffs:
        return 2
    if not updates:
        print("nothing to do")
        return 0
    return None


def preview_updates(updates: list) -> None:
    for ver, hx, name in updates[:5]:
        print(f"  will update {ver} {name}")
    if len(updates) > 5:
        print(f"  ... and {len(updates)-5} more")


def apply_updates(args: argparse.Namespace, updates: list) -> None:
    sql = ["BEGIN;"]
    for ver, hx, _ in updates:
        sql.append(
            f"UPDATE _sqlx_migrations SET checksum = decode('{hx}', 'hex') WHERE version = {ver};"
        )
    sql.append("COMMIT;")
    path = ROOT / ".tmp_realign_checksums.sql"
    path.write_text("\n".join(sql) + "\n", encoding="utf-8", newline="\n")
    subprocess.check_call(
        ["docker", "cp", str(path), f"{args.container}:/tmp/realign_checksums.sql"]
    )
    subprocess.check_call(
        docker_psql(
            args, "-v", "ON_ERROR_STOP=1", "-f", "/tmp/realign_checksums.sql"
        )
    )
    path.unlink(missing_ok=True)
    print(f"updated {len(updates)} checksums")


def main() -> int:
    args = parse_args()
    updates, content_diffs = classify(read_db_checksums(args))
    code = summarize(updates, content_diffs)
    if code is not None:
        return code
    preview_updates(updates)
    if not args.apply:
        print("dry-run only; pass --apply to write")
        return 0
    apply_updates(args, updates)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
