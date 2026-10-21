#!/usr/bin/env python3
"""Does a Spanish lexical channel retrieve better than the 'simple' one on YOUR corpus?

0.32 B1 asks whether `to_tsvector('spanish', <accents folded>)` beside the `'simple'` vector
that `cuba_faro` uses today earns a place in the hybrid search. The backlog's rule is that it is
adopted only if it wins at least 4 points of nDCG@10 on a frozen test, and dropped (and the
result written down) if it does not. The frozen test is `rust/eval-datasets/phase2-id-scored.jsonl`,
whose `relevant_ids` are ids from the live corpus, so the experiment can only run where that corpus
lives. This is the experiment, read-only, so nobody has to build the feature to find out.

What it compares, per query of the dataset, over observations and entities together:

  simple   what production runs: the stored `search_vector` and `cuba_or_tsquery` (migration 0026)
  spanish  the same shape with the 'spanish' configuration and a, e, i, o, u, u-umlaut folded
           (not n: «ano» and «año» are different words)
  fused    both ranked lists fused with reciprocal rank (k = 60), which is how a second channel
           would join the hybrid search

and prints nDCG@10, recall@10 and MRR for each, with the verdict the rule gives. It writes nothing
to the database (only SELECTs) and needs `psql` on the PATH and DATABASE_URL.

    DATABASE_URL=postgresql://... python3 scripts/lexical_ab.py
    python3 scripts/lexical_ab.py --dataset my.jsonl --k 10 --json
    python3 scripts/lexical_ab.py --self-test

Read the result with the sample size next to it: the shipped dataset has 24 queries, and a
difference of four points in a mean over 24 is inside what changing three of them would move.
Exit 0 on success (the verdict is information, not a failure), 2 when it cannot run.
"""

import argparse
import json
import math
import os
import subprocess
import sys
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlparse

RRF_K = 60
ADOPT_IF_GAIN_AT_LEAST = 0.04

FOLD = "translate({x}, 'áéíóúüÁÉÍÓÚÜ', 'aeiouuAEIOUU')"

OBSERVATIONS_SIMPLE = """
SELECT o.id, ts_rank_cd(o.search_vector, cuba_or_tsquery(:'q')) AS r
FROM brain_observations o
WHERE o.search_vector @@ cuba_or_tsquery(:'q')
  AND o.observation_type != 'superseded' AND o.trust = 'trusted'
"""
ENTITIES_SIMPLE = """
SELECT e.id, ts_rank_cd(e.search_vector, cuba_or_tsquery(:'q')) AS r
FROM brain_entities e
WHERE e.search_vector @@ cuba_or_tsquery(:'q')
"""
OBSERVATIONS_SPANISH = """
SELECT o.id, ts_rank_cd(to_tsvector('spanish', {content}), q.tq) AS r
FROM brain_observations o, q
WHERE to_tsvector('spanish', {content}) @@ q.tq
  AND o.observation_type != 'superseded' AND o.trust = 'trusted'
""".format(content=FOLD.format(x="o.content"))
ENTITIES_SPANISH = """
SELECT e.id, ts_rank_cd(to_tsvector('spanish', {text}), q.tq) AS r
FROM brain_entities e, q
WHERE to_tsvector('spanish', {text}) @@ q.tq
""".format(text=FOLD.format(x="coalesce(e.name, '') || ' ' || coalesce(e.entity_type, '')"))

SQL_SIMPLE = (
    "SELECT id FROM (" + OBSERVATIONS_SIMPLE + " UNION ALL " + ENTITIES_SIMPLE + ") t "
    "ORDER BY r DESC, id LIMIT :k;"
)
SQL_SPANISH = (
    "WITH q AS (SELECT replace(plainto_tsquery('spanish', "
    + FOLD.format(x=":'q'")
    + ")::text, ' & ', ' | ')::tsquery AS tq) "
    "SELECT id FROM (" + OBSERVATIONS_SPANISH + " UNION ALL " + ENTITIES_SPANISH + ") t "
    "ORDER BY r DESC, id LIMIT :k;"
)


def dcg(ranked, relevant, k):
    return sum(1.0 / math.log2(i + 2) for i, doc in enumerate(ranked[:k]) if doc in relevant)


def ndcg_at_k(ranked, relevant, k):
    ideal = sum(1.0 / math.log2(i + 2) for i in range(min(len(relevant), k)))
    return dcg(ranked, relevant, k) / ideal if ideal else 0.0


def recall_at_k(ranked, relevant, k):
    return len(set(ranked[:k]) & relevant) / len(relevant) if relevant else 0.0


def reciprocal_rank(ranked, relevant):
    for i, doc in enumerate(ranked):
        if doc in relevant:
            return 1.0 / (i + 1)
    return 0.0


def fuse(*lists, k=RRF_K, limit=None):
    score = {}
    for ranked in lists:
        for rank, doc in enumerate(ranked):
            score[doc] = score.get(doc, 0.0) + 1.0 / (k + rank + 1)
    order = sorted(score, key=lambda d: (-score[d], d))
    return order[:limit] if limit else order


def connection_env(url):
    """psql reads its connection from PG* variables, so the password never reaches `ps`."""
    parsed = urlparse(url)
    if parsed.scheme not in ("postgres", "postgresql"):
        raise ValueError("DATABASE_URL must be a postgres:// or postgresql:// URL")
    env = {
        "PGHOST": parsed.hostname or "localhost",
        "PGPORT": str(parsed.port or 5432),
        "PGDATABASE": unquote(parsed.path.lstrip("/")) or "postgres",
    }
    if parsed.username:
        env["PGUSER"] = unquote(parsed.username)
    if parsed.password:
        env["PGPASSWORD"] = unquote(parsed.password)
    sslmode = parse_qs(parsed.query).get("sslmode")
    if sslmode:
        env["PGSSLMODE"] = sslmode[0]
    return env


def ranking(env, sql, query, k):
    out = subprocess.run(
        ["psql", "-X", "-q", "-A", "-t", "-v", "ON_ERROR_STOP=1", "-v", f"q={query}", "-v", f"k={k}", "-f", "-"],
        input=sql, capture_output=True, text=True, env={**os.environ, **env}, check=False,
    )
    if out.returncode != 0:
        raise RuntimeError(out.stderr.strip() or "psql failed")
    return [line.strip() for line in out.stdout.splitlines() if line.strip()]


def load_dataset(path):
    rows = []
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        ids = {i for i in row.get("relevant_ids") or [] if i}
        if row.get("query") and ids:
            rows.append((row["query"], ids))
    return rows


def evaluate(rows, run, k):
    totals = {name: [0.0, 0.0, 0.0] for name in ("simple", "spanish", "fused")}
    for query, relevant in rows:
        simple = run(SQL_SIMPLE, query, k)
        spanish = run(SQL_SPANISH, query, k)
        for name, ranked in (("simple", simple), ("spanish", spanish), ("fused", fuse(simple, spanish, limit=k))):
            totals[name][0] += ndcg_at_k(ranked, relevant, k)
            totals[name][1] += recall_at_k(ranked, relevant, k)
            totals[name][2] += reciprocal_rank(ranked, relevant)
    n = len(rows)
    return {name: {"ndcg": v[0] / n, "recall": v[1] / n, "mrr": v[2] / n} for name, v in totals.items()}


def verdict(result):
    gain = result["fused"]["ndcg"] - result["simple"]["ndcg"]
    if gain >= ADOPT_IF_GAIN_AT_LEAST:
        return gain, "ADOPT: the fused channel gains {:.1f} points of nDCG@10 (rule: >= 4)".format(gain * 100)
    return gain, "DROP: the fused channel gains {:.1f} points of nDCG@10 (rule: >= 4); write that down".format(gain * 100)


# --- self-test begin ---
def self_test():
    problems = []

    def expect(name, got, want):
        if got != want:
            problems.append(f"{name}: got {got!r}, wanted {want!r}")

    relevant = {"a", "b"}
    expect("ndcg perfect", ndcg_at_k(["a", "b", "x"], relevant, 10), 1.0)
    expect("ndcg none", ndcg_at_k(["x", "y"], relevant, 10), 0.0)
    expect("ndcg second place", round(ndcg_at_k(["x", "a"], {"a"}, 10), 6), round(1 / math.log2(3), 6))
    expect("ndcg capped at k", ndcg_at_k(["a"], {"a", "b", "c"}, 1), 1.0)
    expect("ndcg empty relevant", ndcg_at_k(["a"], set(), 10), 0.0)
    expect("recall half", recall_at_k(["a", "x"], relevant, 10), 0.5)
    expect("recall cut by k", recall_at_k(["x", "a"], {"a"}, 1), 0.0)
    expect("rr third", reciprocal_rank(["x", "y", "a"], {"a"}), 1 / 3)
    expect("rr miss", reciprocal_rank(["x"], {"a"}), 0.0)
    expect("fuse favours agreement", fuse(["a", "b"], ["c", "a"])[0], "a")
    expect("fuse limit", len(fuse(["a", "b"], ["c"], limit=2)), 2)
    expect("fuse deterministic ties", fuse(["b"], ["a"]), ["a", "b"])

    env = connection_env("postgresql://cuba:p%40ss@db.example:6543/brain?sslmode=require")
    expect("url host", env["PGHOST"], "db.example")
    expect("url port", env["PGPORT"], "6543")
    expect("url password decoded", env["PGPASSWORD"], "p@ss")
    expect("url ssl", env["PGSSLMODE"], "require")
    try:
        connection_env("mysql://x/y")
        problems.append("a non-postgres URL was accepted")
    except ValueError:
        pass

    expect("verdict adopt", verdict({"fused": {"ndcg": 0.60}, "simple": {"ndcg": 0.50}})[1][:5], "ADOPT")
    expect("verdict drop below the rule", verdict({"fused": {"ndcg": 0.53}, "simple": {"ndcg": 0.50}})[1][:4], "DROP")
    expect("verdict boundary is inclusive", verdict({"fused": {"ndcg": 0.54}, "simple": {"ndcg": 0.50}})[1][:5], "ADOPT")

    for sql in (SQL_SIMPLE, SQL_SPANISH):
        expect("only reads", any(w in sql.upper() for w in ("INSERT ", "UPDATE ", "DELETE ", "DROP ", "ALTER ", "CREATE ")), False)
    return problems


# --- self-test end ---


def main(argv):
    parser = argparse.ArgumentParser(description="Lexical channel A/B: 'simple' vs Spanish, read-only.")
    default = Path(__file__).resolve().parent.parent / "rust" / "eval-datasets" / "phase2-id-scored.jsonl"
    parser.add_argument("--dataset", default=str(default))
    parser.add_argument("--k", type=int, default=10)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args(argv)

    if args.self_test:
        problems = self_test()
        for p in problems:
            print("FAIL:", p, file=sys.stderr)
        print("lexical_ab self-test:", "FAILED" if problems else "OK")
        return 1 if problems else 0

    url = os.environ.get("DATABASE_URL")
    if not url:
        print("DATABASE_URL is required (read-only: only SELECTs are sent)", file=sys.stderr)
        return 2
    try:
        env = connection_env(url)
        rows = load_dataset(args.dataset)
        if not rows:
            print(f"{args.dataset}: no query with relevant_ids", file=sys.stderr)
            return 2
        result = evaluate(rows, lambda sql, q, k: ranking(env, sql, q, k), args.k)
    except (OSError, RuntimeError, ValueError) as why:
        print(f"cannot run: {why}", file=sys.stderr)
        return 2

    gain, text = verdict(result)
    if args.json:
        print(json.dumps({"queries": len(rows), "k": args.k, "result": result, "gain_ndcg": gain, "verdict": text}))
        return 0
    print(f"{len(rows)} queries with relevant_ids, k={args.k}")
    print(f"{'channel':<10}{'nDCG@' + str(args.k):>10}{'recall@' + str(args.k):>11}{'MRR':>8}")
    for name in ("simple", "spanish", "fused"):
        v = result[name]
        print(f"{name:<10}{v['ndcg']:>10.3f}{v['recall']:>11.3f}{v['mrr']:>8.3f}")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
