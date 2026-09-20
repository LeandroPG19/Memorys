#!/usr/bin/env bash

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <brain_*.dump>" >&2
  exit 1
fi

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DUMP="$1"
DATABASE_URL="${DATABASE_URL:-postgresql://cuba:memorys2026@127.0.0.1:5488/brain}"

if [[ ! -f "$DUMP" ]]; then
  echo "error: dump file not found: $DUMP" >&2
  exit 1
fi

# Same names as rust/src/setup.rs / docker-compose.yml.
pg_container() {
  docker ps --format '{{.Names}}' 2>/dev/null | tr -d '\r' | grep -Ex 'memory-industry-db|cuba-memorys-db' | head -1
}
PG_CONTAINER="$(pg_container || true)"

echo "Validating $DUMP before touching the database ..."

if [[ -n "${PG_CONTAINER:-}" ]]; then
  TOC="$(docker exec -i "$PG_CONTAINER" pg_restore --list <"$DUMP" 2>&1)" || {
    echo "FAIL: this dump is unreadable. Nothing was changed." >&2
    exit 1
  }
elif command -v pg_restore >/dev/null 2>&1; then
  TOC="$(pg_restore --list "$DUMP" 2>&1)" || {
    echo "FAIL: this dump is unreadable. Nothing was changed." >&2
    exit 1
  }
else
  echo "error: pg_restore not found and neither memory-industry-db nor cuba-memorys-db is running." >&2
  exit 1
fi

TOC_TABLES="$(grep -c 'TABLE DATA public' <<<"$TOC" || true)"
if ((TOC_TABLES == 0)); then
  echo "FAIL: the dump carries no table data. Restoring it would empty the database." >&2
  exit 1
fi
echo "OK  the dump is readable and carries data for $TOC_TABLES tables."

echo "Backing up the CURRENT database before replacing it ..."
BACKUP_DIR="$ROOT/backups/pre-restore" "$ROOT/scripts/backup-db.sh" || {
  echo "FAIL: could not back up the current state. Refusing to restore over it." >&2
  exit 1
}

echo
echo "About to replace the contents of $DATABASE_URL with $DUMP."
echo "The restore runs in a single transaction: if it fails, nothing changes."
read -r -p "Continue? [y/N] " ans
if [[ "${ans,,}" != "y" ]]; then
  echo "Aborted."
  exit 0
fi

if [[ -n "${PG_CONTAINER:-}" ]]; then
  echo "Using pg_restore inside container $PG_CONTAINER ..."
  docker exec -i "$PG_CONTAINER" pg_restore -U cuba -d brain \
    --clean \
    --if-exists \
    --no-owner \
    --no-acl \
    --single-transaction \
    --verbose <"$DUMP"
else
  pg_restore \
    --dbname="$DATABASE_URL" \
    --clean \
    --if-exists \
    --no-owner \
    --no-acl \
    --single-transaction \
    --verbose \
    "$DUMP"
fi

echo "Restore finished. The pre-restore snapshot is under $ROOT/backups/pre-restore/."
