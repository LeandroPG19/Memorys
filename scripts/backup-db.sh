#!/usr/bin/env bash

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BACKUP_DIR="${BACKUP_DIR:-$ROOT/backups}"
KEEP_COUNT="${KEEP_COUNT:-7}"
DATABASE_URL="${DATABASE_URL:-postgresql://cuba:memorys2026@127.0.0.1:5488/brain}"

COUNTED_TABLES=(brain_observations brain_entities brain_relations brain_episodes brain_audit_log)

# Same names as rust/src/setup.rs. compose.yml is memory-industry-db;
# cuba-memorys-db is the pre-rebrand container. Host pg_dump is 16 and
# cannot dump the PG 18 cluster — that path is last resort only.
pg_container() {
  docker ps --format '{{.Names}}' 2>/dev/null | tr -d '\r' | grep -Ex 'memory-industry-db|cuba-memorys-db' | head -1
}

mkdir -p "$BACKUP_DIR"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="$BACKUP_DIR/brain_${STAMP}.dump"
PART="$OUT.part"
META="$BACKUP_DIR/brain_${STAMP}.meta.json"

PG_CONTAINER="$(pg_container || true)"

run_psql() {
  if [[ -n "${PG_CONTAINER:-}" ]]; then
    docker exec "$PG_CONTAINER" psql -U cuba -d brain -Atc "$1"
  else
    psql "$DATABASE_URL" -Atc "$1"
  fi
}

cleanup_part() {
  rm -f "$PART"
}
trap cleanup_part EXIT

echo "Backing up database to $OUT ..."

declare -A LIVE_ROWS
for t in "${COUNTED_TABLES[@]}"; do
  LIVE_ROWS[$t]="$(run_psql "SELECT COUNT(*) FROM $t" 2>/dev/null || echo -1)"
done

# The container's pg_dump is used because the host's is usually older than the
# server (16 against 18.3 here), and pg_dump refuses to dump a newer server.
# The cost: if this docker exec is interrupted, pg_dump is orphaned and adopted
# by the postmaster, and an adopted child exiting non-zero makes the postmaster
# restart the whole cluster. Measured 2026-08-14 — recovery is automatic and
# nothing was lost, but do not kill a running backup.
if [[ -n "${PG_CONTAINER:-}" ]]; then
  echo "Using pg_dump from container $PG_CONTAINER (PG18)."
  docker exec "$PG_CONTAINER" pg_dump -U cuba -d brain \
    --format=custom --no-owner --no-acl >"$PART"
elif command -v pg_dump >/dev/null 2>&1; then
  echo "Using host pg_dump."
  pg_dump --format=custom --no-owner --no-acl --file="$PART" "$DATABASE_URL"
else
  echo "error: no pg_dump and neither memory-industry-db nor cuba-memorys-db is running." >&2
  exit 1
fi

echo "Verifying the dump before trusting it ..."

if [[ -n "${PG_CONTAINER:-}" ]]; then
  TOC="$(docker exec -i "$PG_CONTAINER" pg_restore --list <"$PART" 2>&1)" || {
    echo "FAIL: pg_restore --list cannot read the dump. It is unusable." >&2
    exit 1
  }
elif command -v pg_restore >/dev/null 2>&1; then
  TOC="$(pg_restore --list "$PART" 2>&1)" || {
    echo "FAIL: pg_restore --list cannot read the dump. It is unusable." >&2
    exit 1
  }
else
  echo "FAIL: no pg_restore available; refusing to keep an unverified dump." >&2
  exit 1
fi

MISSING=()
for t in "${COUNTED_TABLES[@]}"; do
  if [[ "${LIVE_ROWS[$t]}" != "0" ]] && ! grep -q "TABLE DATA public $t" <<<"$TOC"; then
    MISSING+=("$t")
  fi
done

if ((${#MISSING[@]} > 0)); then
  echo "FAIL: the dump has no table data for: ${MISSING[*]}" >&2
  echo "      Those tables are not empty in the live database." >&2
  exit 1
fi

TOC_TABLES="$(grep -c 'TABLE DATA public' <<<"$TOC" || true)"
SIZE_BYTES="$(stat -c%s "$PART" 2>/dev/null || stat -f%z "$PART")"

mv "$PART" "$OUT"
trap - EXIT

ROWS_JSON=""
for t in "${COUNTED_TABLES[@]}"; do
  [[ -n "$ROWS_JSON" ]] && ROWS_JSON+=","
  ROWS_JSON+=$'\n    "'"$t"'": '"${LIVE_ROWS[$t]}"
done

cat >"$META" <<EOF
{
  "created_at_utc": "$STAMP",
  "database": "brain",
  "dump_file": "$(basename "$OUT")",
  "size_bytes": $SIZE_BYTES,
  "verified": true,
  "toc_tables_with_data": $TOC_TABLES,
  "rows_at_backup_time": {$ROWS_JSON
  },
  "tool": "pg_dump",
  "format": "custom"
}
EOF

echo "Verified: $TOC_TABLES tables carry data in the dump."
for t in "${COUNTED_TABLES[@]}"; do
  echo "  $t: ${LIVE_ROWS[$t]} rows"
done
echo "Wrote metadata $META"
echo "Backup size: $SIZE_BYTES bytes ($(du -h "$OUT" | cut -f1))"

VERIFIED=()
while IFS= read -r f; do
  [[ -z "$f" ]] && continue
  meta="${f%.dump}.meta.json"
  if [[ -f "$meta" ]] && grep -q '"verified": true' "$meta"; then
    VERIFIED+=("$f")
  fi
done < <(ls -1t "$BACKUP_DIR"/brain_*.dump 2>/dev/null || true)

if ((${#VERIFIED[@]} > KEEP_COUNT)); then
  for f in "${VERIFIED[@]:KEEP_COUNT}"; do
    base="${f%.dump}"
    rm -f "$f" "${base}.meta.json"
    echo "Pruned old verified backup: $(basename "$f")"
  done
fi

echo "Done. Latest backup: $OUT"
