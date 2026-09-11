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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--container", default="cuba-memorys-db")
    ap.add_argument("--db", default="brain")
    ap.add_argument("--user", default="cuba")
    ap.add_argument("--apply", action="store_true")
    args = ap.parse_args()

    out = subprocess.check_output(
        [
            "docker",
            "exec",
            args.container,
            "psql",
            "-U",
            args.user,
            "-d",
            args.db,
            "-t",
            "-A",
            "-F",
            "|",
            "-c",
            "SELECT version, encode(checksum,'hex') FROM _sqlx_migrations ORDER BY version",
        ],
        text=True,
    )
    db = {}
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        ver_s, hx = line.split("|", 1)
        db[int(ver_s)] = hx

    updates = []
    content_diffs = []
    for ver, hx in sorted(db.items()):
        ups = list(MIG.glob(f"{ver:04d}_*.up.sql"))
        if not ups:
            print(f"WARN: version {ver} in DB but no file", file=sys.stderr)
            continue
        data = ups[0].read_bytes()
        lf = sha384(data.replace(b"\r\n", b"\n"))
        crlf = sha384(data.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n"))
        as_is = sha384(data)
        if hx == as_is:
            continue
        if hx in (lf, crlf) or hx == as_is:
            # line-ending only (or already matching after normalize)
            updates.append((ver, as_is, ups[0].name))
        else:
            content_diffs.append((ver, ups[0].name))

    print(f"line-ending drift: {len(updates)}")
    print(f"content diffs (refused): {len(content_diffs)}")
    for ver, name in content_diffs[:20]:
        print(f"  CONTENT {ver} {name}")
    if content_diffs:
        return 2
    if not updates:
        print("nothing to do")
        return 0
    for ver, hx, name in updates[:5]:
        print(f"  will update {ver} {name}")
    if len(updates) > 5:
        print(f"  ... and {len(updates)-5} more")
    if not args.apply:
        print("dry-run only; pass --apply to write")
        return 0

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
        [
            "docker",
            "exec",
            args.container,
            "psql",
            "-U",
            args.user,
            "-d",
            args.db,
            "-v",
            "ON_ERROR_STOP=1",
            "-f",
            "/tmp/realign_checksums.sql",
        ]
    )
    path.unlink(missing_ok=True)
    print(f"updated {len(updates)} checksums")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
