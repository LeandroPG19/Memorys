//! Optional graph engine (FalkorDB / Neo4j) projected from Postgres SoT.
//! Writes use GRAPH.QUERY; reads use GRAPH.RO_QUERY and parse the RESP table.

use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::graph::paths::{PATHRAG_LT_K, PATHRAG_LT_N, RelPath, path_score, prune_paths};

static PROJECTED: AtomicU64 = AtomicU64::new(0);
static LAST_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

const DEFAULT_GRAPH_NAME: &str = "memory_industry";
const RESP_MAX: usize = 8 * 1024 * 1024;
const RECONCILE_PAGE: i64 = 250;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Off,
    Falkor,
    Neo4j,
}

#[derive(Debug, Clone, PartialEq)]
enum RespVal {
    Nil,
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Vec<u8>),
    Array(Vec<RespVal>),
}

impl RespVal {
    fn as_str(&self) -> Option<String> {
        match self {
            RespVal::Simple(s) | RespVal::Error(s) => Some(s.clone()),
            RespVal::Bulk(b) => String::from_utf8(b.clone()).ok(),
            RespVal::Int(i) => Some(i.to_string()),
            RespVal::Nil => None,
            RespVal::Array(_) => None,
        }
    }
}

pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    #[cfg(windows)]
    let base = PathBuf::from(&home)
        .join("AppData")
        .join("Roaming")
        .join("memory-industry");
    #[cfg(not(windows))]
    let base = PathBuf::from(&home).join(".config").join("memory-industry");
    Some(base.join("graph.env"))
}

pub fn load_saved_config_into_env() {
    let Some(path) = config_path() else {
        return;
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim();
        let v = v.trim().trim_matches('"');
        if k.is_empty() {
            continue;
        }
        if std::env::var_os(k).is_none() {
            unsafe {
                std::env::set_var(k, v);
            }
        }
    }
}

pub fn backend() -> Backend {
    match std::env::var("MEMORY_INDUSTRY_GRAPH_DB")
        .or_else(|_| std::env::var("CUBA_GRAPH_DB"))
        .unwrap_or_else(|_| "off".into())
        .to_lowercase()
        .as_str()
    {
        "falkor" | "falkordb" | "redisgraph" => Backend::Falkor,
        "neo4j" => Backend::Neo4j,
        _ => Backend::Off,
    }
}

pub fn graph_url() -> Option<String> {
    std::env::var("MEMORY_INDUSTRY_GRAPH_URL")
        .or_else(|_| std::env::var("CUBA_GRAPH_URL"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

pub fn graph_name() -> String {
    let raw = std::env::var("MEMORY_INDUSTRY_GRAPH_NAME")
        .or_else(|_| std::env::var("CUBA_GRAPH_NAME"))
        .unwrap_or_else(|_| DEFAULT_GRAPH_NAME.into());
    let cleaned: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    if cleaned.is_empty() {
        DEFAULT_GRAPH_NAME.into()
    } else {
        cleaned
    }
}

fn record_err(e: impl ToString) {
    if let Ok(mut g) = LAST_ERROR.lock() {
        *g = Some(e.to_string());
    }
}

fn clear_err() {
    if let Ok(mut g) = LAST_ERROR.lock() {
        *g = None;
    }
}

fn escape_cypher_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn resp_bulk(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("${}\r\n", s.len()).as_bytes());
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(b"\r\n");
    out
}

fn resp_array(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("*{}\r\n", parts.len()).as_bytes());
    for p in parts {
        out.extend(resp_bulk(p));
    }
    out
}

fn redis_hostport(url: &str) -> Option<&str> {
    url.strip_prefix("redis://")
        .or_else(|| url.strip_prefix("falkor://"))
        .map(|s| s.trim_end_matches('/'))
}

fn parse_resp(input: &[u8]) -> Result<(RespVal, usize), String> {
    if input.is_empty() {
        return Err("empty".into());
    }
    match input[0] {
        b'+' | b'-' | b':' => {
            let nl = find_crlf(input, 1).ok_or("incomplete simple")?;
            let text = std::str::from_utf8(&input[1..nl]).map_err(|e| e.to_string())?;
            let val = match input[0] {
                b'+' => RespVal::Simple(text.to_string()),
                b'-' => RespVal::Error(text.to_string()),
                _ => RespVal::Int(
                    text.parse()
                        .map_err(|e: std::num::ParseIntError| e.to_string())?,
                ),
            };
            Ok((val, nl + 2))
        }
        b'$' => {
            let nl = find_crlf(input, 1).ok_or("incomplete bulk len")?;
            let len: isize = std::str::from_utf8(&input[1..nl])
                .map_err(|e| e.to_string())?
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
            if len < 0 {
                return Ok((RespVal::Nil, nl + 2));
            }
            let start = nl + 2;
            let end = start + len as usize;
            if input.len() < end + 2 {
                return Err("incomplete bulk".into());
            }
            Ok((RespVal::Bulk(input[start..end].to_vec()), end + 2))
        }
        b'*' => {
            let nl = find_crlf(input, 1).ok_or("incomplete array len")?;
            let n: isize = std::str::from_utf8(&input[1..nl])
                .map_err(|e| e.to_string())?
                .parse()
                .map_err(|e: std::num::ParseIntError| e.to_string())?;
            let mut pos = nl + 2;
            if n < 0 {
                return Ok((RespVal::Nil, pos));
            }
            let mut items = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let (v, used) = parse_resp(&input[pos..])?;
                pos += used;
                items.push(v);
            }
            Ok((RespVal::Array(items), pos))
        }
        other => Err(format!("bad resp type {}", other as char)),
    }
}

fn find_crlf(input: &[u8], from: usize) -> Option<usize> {
    input[from..]
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|p| from + p)
}

fn read_resp(stream: &mut TcpStream) -> Result<RespVal, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        let n = stream
            .read(&mut tmp)
            .map_err(|e| format!("RESP read: {e}"))?;
        if n == 0 {
            return Err("RESP connection closed".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > RESP_MAX {
            return Err("RESP reply too large".into());
        }
        if let Ok((val, used)) = parse_resp(&buf)
            && used > 0
        {
            return Ok(val);
        }
    }
}

/// Rows of a Falkor/RedisGraph table: header + records, flattened to strings.
fn table_rows(reply: &RespVal) -> Result<Vec<Vec<String>>, String> {
    if let RespVal::Error(e) = reply {
        return Err(e.clone());
    }
    let RespVal::Array(top) = reply else {
        return Err("graph reply is not an array".into());
    };
    if top.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(RespVal::Error(e)) = top.first() {
        return Err(e.clone());
    }
    // Compact / standard: [header, records, stats?]
    let records = if top.len() >= 2 { &top[1] } else { &top[0] };
    let RespVal::Array(rows) = records else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for row in rows {
        match row {
            RespVal::Array(cells) => {
                out.push(cells.iter().filter_map(flatten_cell).collect());
            }
            other => {
                if let Some(s) = other.as_str() {
                    out.push(vec![s]);
                }
            }
        }
    }
    Ok(out)
}

fn flatten_cell(v: &RespVal) -> Option<String> {
    match v {
        RespVal::Array(inner) => {
            // compact typed value: [type_tag, payload]
            inner.iter().rev().find_map(flatten_cell)
        }
        _ => v.as_str(),
    }
}

fn falkor_exec(url: &str, cmd: &str, cypher: &str) -> Result<RespVal, String> {
    let hostport = redis_hostport(url).ok_or_else(|| "not a redis/falkor url".to_string())?;
    let mut stream =
        TcpStream::connect(hostport).map_err(|e| format!("connect {hostport}: {e}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(8))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(8))).ok();
    let name = graph_name();
    let wire = resp_array(&[cmd, &name, cypher, "--compact"]);
    stream
        .write_all(&wire)
        .map_err(|e| format!("{cmd} write: {e}"))?;
    let reply = read_resp(&mut stream)?;
    if let RespVal::Error(e) = &reply {
        return Err(format!("{cmd} error: {e}"));
    }
    Ok(reply)
}

fn falkor_query(url: &str, cypher: &str) -> Result<(), String> {
    let _ = falkor_exec(url, "GRAPH.QUERY", cypher)?;
    Ok(())
}

fn falkor_ro_query(url: &str, cypher: &str) -> Result<Vec<Vec<String>>, String> {
    match falkor_exec(url, "GRAPH.RO_QUERY", cypher) {
        Ok(reply) => table_rows(&reply),
        Err(e) => {
            // Older Falkor builds may lack RO_QUERY; fall back to QUERY.
            if e.contains("unknown") || e.contains("ERR") && e.contains("RO_QUERY") {
                let reply = falkor_exec(url, "GRAPH.QUERY", cypher)?;
                return table_rows(&reply);
            }
            // Some servers reply with a nested error string.
            let reply = falkor_exec(url, "GRAPH.QUERY", cypher)?;
            table_rows(&reply).map_err(|inner| format!("{e}; query fallback: {inner}"))
        }
    }
}

/// Probe connectivity. For redis:// / falkor:// uses RESP PING.
pub fn probe_reachable() -> Result<(), String> {
    let Some(url) = graph_url() else {
        return Err("MEMORY_INDUSTRY_GRAPH_URL unset".into());
    };
    if let Some(hostport) = redis_hostport(&url) {
        let mut stream =
            TcpStream::connect(hostport).map_err(|e| format!("connect {hostport}: {e}"))?;
        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
        stream
            .write_all(b"*1\r\n$4\r\nPING\r\n")
            .map_err(|e| format!("PING write: {e}"))?;
        let reply = read_resp(&mut stream).map_err(|e| format!("PING read: {e}"))?;
        let text = reply.as_str().unwrap_or_default();
        if text.contains("PONG") {
            clear_err();
            return Ok(());
        }
        return Err(format!("unexpected PING reply: {text}"));
    }
    Ok(())
}

pub fn status_summary() -> Value {
    let be = backend();
    let reachable = if be == Backend::Off {
        None
    } else {
        Some(probe_reachable().is_ok())
    };
    if let Some(false) = reachable {
        let _ = probe_reachable().map_err(record_err);
    }
    serde_json::json!({
        "backend": match be {
            Backend::Off => "off",
            Backend::Falkor => "falkor",
            Backend::Neo4j => "neo4j",
        },
        "graph_name": graph_name(),
        "url_configured": graph_url().is_some(),
        "reachable": reachable,
        "projected_ops": PROJECTED.load(Ordering::Relaxed),
        "last_error": LAST_ERROR.lock().ok().and_then(|g| g.clone()),
        "note": "Postgres remains source of truth; graph is a query projection"
    })
}

async fn project_cypher(cypher: &str) -> anyhow::Result<()> {
    if backend() == Backend::Off {
        return Ok(());
    }
    let Some(url) = graph_url() else {
        record_err("MEMORY_INDUSTRY_GRAPH_URL unset");
        anyhow::bail!("graph url unset");
    };

    if redis_hostport(&url).is_some() {
        falkor_query(&url, cypher).map_err(|e| {
            record_err(&e);
            anyhow::anyhow!(e)
        })?;
        PROJECTED.fetch_add(1, Ordering::Relaxed);
        clear_err();
        return Ok(());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;
    let body = serde_json::json!({ "query": cypher });
    match client.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => {
            PROJECTED.fetch_add(1, Ordering::Relaxed);
            clear_err();
            Ok(())
        }
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            record_err(format!("graph HTTP {status}: {text}"));
            anyhow::bail!("graph project failed: {status}");
        }
        Err(e) => {
            record_err(e.to_string());
            Err(e.into())
        }
    }
}

pub async fn project_artifact(path: &str, version: i64) -> anyhow::Result<()> {
    let p = escape_cypher_str(path);
    project_cypher(&format!(
        "MERGE (a:Artifact {{path: '{p}'}}) SET a.version = {version}"
    ))
    .await
}

pub async fn project_entity(name: &str, entity_type: &str) -> anyhow::Result<()> {
    project_entity_full(name, entity_type, None).await
}

async fn project_entity_full(
    name: &str,
    entity_type: &str,
    project_id: Option<&str>,
) -> anyhow::Result<()> {
    let n = escape_cypher_str(name);
    let t = escape_cypher_str(entity_type);
    let extra = match project_id {
        Some(p) if !p.is_empty() => format!(", e.project_id = '{}'", escape_cypher_str(p)),
        _ => String::new(),
    };
    project_cypher(&format!(
        "MERGE (e:Entity {{name: '{n}'}}) SET e.entity_type = '{t}'{extra}"
    ))
    .await
}

pub async fn project_relation(from: &str, to: &str, rel_type: &str) -> anyhow::Result<()> {
    project_relation_full(from, to, rel_type, 1.0).await
}

pub async fn project_relation_full(
    from: &str,
    to: &str,
    rel_type: &str,
    strength: f64,
) -> anyhow::Result<()> {
    let f = escape_cypher_str(from);
    let t = escape_cypher_str(to);
    let r = escape_cypher_str(rel_type);
    let s = strength.clamp(0.0, 1.0);
    project_cypher(&format!(
        "MERGE (a:Entity {{name: '{f}'}}) \
         MERGE (b:Entity {{name: '{t}'}}) \
         MERGE (a)-[rel:REL {{kind: '{r}'}}]->(b) SET rel.strength = {s}"
    ))
    .await
}

pub async fn unproject_relation(from: &str, to: &str, rel_type: &str) -> anyhow::Result<()> {
    let f = escape_cypher_str(from);
    let t = escape_cypher_str(to);
    let r = escape_cypher_str(rel_type);
    project_cypher(&format!(
        "MATCH (a:Entity {{name: '{f}'}})-[rel:REL {{kind: '{r}'}}]->(b:Entity {{name: '{t}'}}) DELETE rel"
    ))
    .await
}

#[derive(Debug, Clone)]
pub struct GraphHop {
    pub name: String,
    pub relation: String,
    pub strength: f64,
    pub depth: i32,
}

fn falkor_neighbors(start: &str, incoming: bool) -> Result<Vec<(String, String, f64)>, String> {
    if backend() != Backend::Falkor {
        return Err("graph backend is not falkor".into());
    }
    let Some(url) = graph_url() else {
        return Err("MEMORY_INDUSTRY_GRAPH_URL unset".into());
    };
    let n = escape_cypher_str(start);
    let cypher = if incoming {
        format!(
            "MATCH (n:Entity)-[r:REL]->(s:Entity {{name: '{n}'}}) \
             RETURN n.name, r.kind, coalesce(r.strength, 1.0)"
        )
    } else {
        format!(
            "MATCH (s:Entity {{name: '{n}'}})-[r:REL]->(n:Entity) \
             RETURN n.name, r.kind, coalesce(r.strength, 1.0)"
        )
    };
    let rows = falkor_ro_query(&url, &cypher)?;
    let mut out = Vec::new();
    for row in rows {
        if row.len() < 2 {
            continue;
        }
        let name = row[0].trim().to_string();
        if name.is_empty() {
            continue;
        }
        let kind = row[1].clone();
        let strength = row
            .get(2)
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(1.0);
        out.push((name, kind, strength));
    }
    Ok(out)
}

/// k-hop BFS on Falkor. Empty or error → caller falls back to Postgres.
pub fn traverse_falkor(start: &str, max_depth: i32) -> Result<Vec<GraphHop>, String> {
    Ok(walk_falkor(start, max_depth)?.0)
}

pub fn traverse_paths_falkor(start: &str, max_depth: i32) -> Result<Vec<RelPath>, String> {
    Ok(walk_falkor(start, max_depth)?.1)
}

pub fn traverse_falkor_with_paths(
    start: &str,
    max_depth: i32,
) -> Result<(Vec<GraphHop>, Vec<RelPath>), String> {
    walk_falkor(start, max_depth)
}

pub fn compact_rel_path(p: &RelPath) -> Value {
    serde_json::json!({
        "n": p.nodes,
        "r": p.relations,
        "s": (p.score * 1000.0).round() / 1000.0
    })
}

pub fn hop_ball_payload(hops: &[GraphHop]) -> Value {
    serde_json::json!(
        hops.iter()
            .map(|h| serde_json::json!({
                "name": h.name,
                "relation": h.relation,
                "strength": h.strength,
                "depth": h.depth
            }))
            .collect::<Vec<_>>()
    )
}

#[derive(Debug, Clone)]
pub struct PprNode {
    pub name: String,
    pub score: f64,
}

/// HippoRAG-style personalized PageRank over a local edge list.
/// Teleport mass stays on `seeds` (Gutiérrez et al., arXiv:2405.14831).
pub fn rank_ppr_from_edges(
    edges: &[(String, String, f64)],
    seeds: &[String],
    top_k: usize,
) -> Vec<PprNode> {
    if seeds.is_empty() || top_k == 0 {
        return Vec::new();
    }
    let mut names: Vec<String> = Vec::new();
    let mut idx: HashMap<String, usize> = HashMap::new();
    let mut push = |n: &str| {
        if !idx.contains_key(n) {
            idx.insert(n.to_string(), names.len());
            names.push(n.to_string());
        }
    };
    for s in seeds {
        push(s);
    }
    for (a, b, _) in edges {
        push(a);
        push(b);
    }
    let n = names.len();
    let mut outgoing = vec![vec![]; n];
    for (a, b, w) in edges {
        let i = idx[a];
        let j = idx[b];
        outgoing[i].push((j, *w));
    }
    let seed_idx: Vec<usize> = seeds.iter().filter_map(|s| idx.get(s).copied()).collect();
    let ranks = crate::graph::pagerank::personalized(&outgoing, &seed_idx);
    let mut ranked: Vec<PprNode> = names
        .into_iter()
        .zip(ranks)
        .map(|(name, score)| PprNode { name, score })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(top_k);
    ranked
}

/// Walk Falkor both ways from seed names, then rank with PPR.
pub fn ppr_around_seeds(
    seeds: &[String],
    max_depth: i32,
    top_k: usize,
) -> Result<Vec<PprNode>, String> {
    if backend() != Backend::Falkor {
        return Err("graph backend is not falkor".into());
    }
    if seeds.is_empty() {
        return Ok(Vec::new());
    }
    let max_depth = max_depth.clamp(1, 5);
    let mut visited = HashSet::new();
    let mut q: VecDeque<(String, i32)> = VecDeque::new();
    for s in seeds {
        let t = s.trim();
        if t.is_empty() {
            continue;
        }
        if visited.insert(t.to_string()) {
            q.push_back((t.to_string(), 0));
        }
    }
    let mut seen_edge: HashSet<(String, String)> = HashSet::new();
    let mut edges: Vec<(String, String, f64)> = Vec::new();
    while let Some((node, depth)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        for incoming in [false, true] {
            let neigh = falkor_neighbors(&node, incoming)?;
            for (name, _kind, strength) in neigh {
                let (a, b) = if incoming {
                    (name.clone(), node.clone())
                } else {
                    (node.clone(), name.clone())
                };
                if seen_edge.insert((a.clone(), b.clone())) {
                    edges.push((a, b, strength));
                }
                if visited.insert(name.clone()) {
                    q.push_back((name, depth + 1));
                }
            }
        }
        if visited.len() >= 80 {
            break;
        }
    }
    Ok(rank_ppr_from_edges(&edges, seeds, top_k))
}

type WalkFrontier = (String, i32, Vec<String>, Vec<String>, Vec<f64>);

fn walk_falkor(start: &str, max_depth: i32) -> Result<(Vec<GraphHop>, Vec<RelPath>), String> {
    let max_depth = max_depth.clamp(1, 5);
    let mut hops = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(start.to_string());
    let mut paths: Vec<RelPath> = Vec::new();
    let mut q: VecDeque<WalkFrontier> = VecDeque::new();
    q.push_back((
        start.to_string(),
        0,
        vec![start.to_string()],
        vec![],
        vec![],
    ));

    while let Some((node, depth, nodes, rels, strengths)) = q.pop_front() {
        if depth >= max_depth {
            continue;
        }
        let neigh = falkor_neighbors(&node, false)?;
        for (name, kind, strength) in neigh {
            if !seen.insert(name.clone()) {
                continue;
            }
            let d = depth + 1;
            hops.push(GraphHop {
                name: name.clone(),
                relation: kind.clone(),
                strength,
                depth: d,
            });
            let mut n2 = nodes.clone();
            n2.push(name.clone());
            let mut r2 = rels.clone();
            r2.push(kind);
            let mut s2 = strengths.clone();
            s2.push(strength);
            paths.push(RelPath {
                nodes: n2.clone(),
                relations: r2.clone(),
                strengths: s2.clone(),
                score: path_score(&s2, d as usize),
            });
            q.push_back((name, d, n2, r2, s2));
            if hops.len() >= 50 {
                break;
            }
        }
        if hops.len() >= 50 {
            break;
        }
    }
    Ok((hops, prune_paths(paths, PATHRAG_LT_N, PATHRAG_LT_K)))
}

pub struct HypothesisHop {
    pub name: String,
    pub relation: String,
    pub strength: f64,
    pub depth: i32,
}

pub fn explain_falkor(
    effect: &str,
    max_depth: i32,
    limit: usize,
) -> Result<Vec<HypothesisHop>, String> {
    let max_depth = max_depth.clamp(1, 5);
    let kinds = ["causes", "related_to", "depends_on"];
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(effect.to_string());
    let mut layer = vec![effect.to_string()];
    for depth in 1..=max_depth {
        let mut next = Vec::new();
        for node in &layer {
            let neigh = falkor_neighbors(node, true)?;
            for (name, kind, strength) in neigh {
                if !kinds.contains(&kind.as_str()) {
                    continue;
                }
                if !seen.insert(name.clone()) {
                    continue;
                }
                out.push(HypothesisHop {
                    name: name.clone(),
                    relation: kind,
                    strength,
                    depth,
                });
                next.push(name);
                if out.len() >= limit {
                    return Ok(out);
                }
            }
        }
        layer = next;
        if layer.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// Full reconcile: page through PG (no 500-row cap).
pub async fn reconcile(pool: &sqlx::PgPool) -> Value {
    let be = backend();
    if be == Backend::Off {
        return serde_json::json!({
            "ok": false,
            "error": "graph backend off",
            "hint": "memory-industry graph set falkor",
        });
    }
    let reach = probe_reachable();
    if let Err(e) = &reach {
        return serde_json::json!({
            "ok": false,
            "error": e,
            "backend": status_summary(),
        });
    }

    let mut projected_entities = 0u32;
    let mut postgres_entities = 0u32;
    let mut last_name = String::new();
    loop {
        let page: Vec<(String, String)> = sqlx::query_as(
            "SELECT name, entity_type FROM brain_entities \
             WHERE name > $1 ORDER BY name ASC LIMIT $2",
        )
        .bind(&last_name)
        .bind(RECONCILE_PAGE)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        if page.is_empty() {
            break;
        }
        postgres_entities += page.len() as u32;
        for (name, etype) in &page {
            if project_entity(name, etype).await.is_ok() {
                projected_entities += 1;
            }
            last_name = name.clone();
        }
        if page.len() < RECONCILE_PAGE as usize {
            break;
        }
    }

    let mut projected_relations = 0u32;
    let mut postgres_relations = 0u32;
    let mut last_id = uuid::Uuid::nil();
    loop {
        let page: Vec<(String, String, String, f64)> = sqlx::query_as(
            "SELECT e1.name, e2.name, r.relation_type, r.strength::float8 \
             FROM brain_relations r \
             JOIN brain_entities e1 ON e1.id = r.from_entity \
             JOIN brain_entities e2 ON e2.id = r.to_entity \
             WHERE r.id > $1 ORDER BY r.id ASC LIMIT $2",
        )
        .bind(last_id)
        .bind(RECONCILE_PAGE)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        if page.is_empty() {
            break;
        }
        postgres_relations += page.len() as u32;
        let ids: Vec<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT r.id FROM brain_relations r WHERE r.id > $1 ORDER BY r.id ASC LIMIT $2",
        )
        .bind(last_id)
        .bind(RECONCILE_PAGE)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        for ((from, to, kind, strength), (id,)) in page.iter().zip(ids.iter()) {
            if project_relation_full(from, to, kind, *strength)
                .await
                .is_ok()
            {
                projected_relations += 1;
            }
            last_id = *id;
        }
        if page.len() < RECONCILE_PAGE as usize {
            break;
        }
    }

    let mut projected_artifacts = 0u32;
    let mut postgres_artifacts = 0u32;
    let mut last_path = String::new();
    loop {
        let page: Vec<(String, i64)> = sqlx::query_as(
            "SELECT path, version FROM brain_artifacts \
             WHERE path > $1 ORDER BY path ASC LIMIT $2",
        )
        .bind(&last_path)
        .bind(RECONCILE_PAGE)
        .fetch_all(pool)
        .await
        .unwrap_or_default();
        if page.is_empty() {
            break;
        }
        postgres_artifacts += page.len() as u32;
        for (path, ver) in &page {
            if project_artifact(path, *ver).await.is_ok() {
                projected_artifacts += 1;
            }
            last_path = path.clone();
        }
        if page.len() < RECONCILE_PAGE as usize {
            break;
        }
    }

    crate::events::publish(
        "graph.reconciled",
        serde_json::json!({
            "entities": projected_entities,
            "relations": projected_relations,
            "artifacts": projected_artifacts,
        }),
    );

    serde_json::json!({
        "ok": true,
        "backend": status_summary(),
        "projected_entities": projected_entities,
        "projected_relations": projected_relations,
        "projected_artifacts": projected_artifacts,
        "postgres_entities": postgres_entities,
        "postgres_relations": postgres_relations,
        "postgres_artifacts": postgres_artifacts,
        "paged": true,
    })
}

pub async fn reconcile_stub(pool: &sqlx::PgPool) -> Value {
    reconcile(pool).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cypher_escape_quotes() {
        assert_eq!(escape_cypher_str("a'b"), "a\\'b");
    }

    #[test]
    fn resp_array_shapes_ping() {
        let b = resp_array(&["PING"]);
        assert!(String::from_utf8_lossy(&b).contains("PING"));
    }

    #[test]
    fn parse_simple_error_and_int() {
        let (v, _) = parse_resp(b"+PONG\r\n").unwrap();
        assert_eq!(v, RespVal::Simple("PONG".into()));
        let (v, _) = parse_resp(b"-ERR boom\r\n").unwrap();
        assert_eq!(v, RespVal::Error("ERR boom".into()));
        let (v, _) = parse_resp(b":7\r\n").unwrap();
        assert_eq!(v, RespVal::Int(7));
    }

    #[test]
    fn parse_bulk_and_array() {
        let (v, n) = parse_resp(b"$4\r\nPONG\r\n").unwrap();
        assert_eq!(v.as_str().as_deref(), Some("PONG"));
        assert_eq!(n, 10);
        let raw = b"*2\r\n$3\r\nfoo\r\n$3\r\nbar\r\n";
        let (v, _) = parse_resp(raw).unwrap();
        let RespVal::Array(items) = v else {
            panic!("array")
        };
        assert_eq!(items[0].as_str().as_deref(), Some("foo"));
        assert_eq!(items[1].as_str().as_deref(), Some("bar"));
    }

    #[test]
    fn table_rows_from_header_records_stats() {
        // [header, records, stats]
        let records = RespVal::Array(vec![RespVal::Array(vec![
            RespVal::Bulk(b"Alpha".to_vec()),
            RespVal::Bulk(b"causes".to_vec()),
            RespVal::Bulk(b"0.8".to_vec()),
        ])]);
        let reply = RespVal::Array(vec![
            RespVal::Array(vec![RespVal::Bulk(b"name".to_vec())]),
            records,
            RespVal::Array(vec![RespVal::Bulk(b"Cached execution".to_vec())]),
        ]);
        let rows = table_rows(&reply).unwrap();
        assert_eq!(rows[0], vec!["Alpha", "causes", "0.8"]);
    }

    #[test]
    fn graph_name_strips_junk_and_defaults() {
        let prev = std::env::var("MEMORY_INDUSTRY_GRAPH_NAME").ok();
        unsafe { std::env::remove_var("MEMORY_INDUSTRY_GRAPH_NAME") };
        unsafe { std::env::remove_var("CUBA_GRAPH_NAME") };
        assert_eq!(graph_name(), "memory_industry");
        unsafe { std::env::set_var("MEMORY_INDUSTRY_GRAPH_NAME", "brain_gate!") };
        assert_eq!(graph_name(), "brain_gate");
        match prev {
            Some(v) => unsafe { std::env::set_var("MEMORY_INDUSTRY_GRAPH_NAME", v) },
            None => unsafe { std::env::remove_var("MEMORY_INDUSTRY_GRAPH_NAME") },
        }
    }

    #[test]
    fn table_rows_surface_graph_errors() {
        let err = table_rows(&RespVal::Error("ERR syntax".into()));
        assert!(err.unwrap_err().contains("syntax"));
    }

    #[test]
    fn ppr_from_edges_keeps_mass_near_seed() {
        let edges = vec![("A".into(), "B".into(), 1.0), ("B".into(), "C".into(), 1.0)];
        let ranked = rank_ppr_from_edges(&edges, &["A".into()], 3);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].name, "A");
        let pos = |n: &str| ranked.iter().position(|x| x.name == n).unwrap();
        assert!(pos("A") < pos("C"));
        assert!(ranked[0].score > ranked[2].score);
    }
}
