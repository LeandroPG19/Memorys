use std::time::Instant;

#[derive(Clone, Copy, Default)]
struct Mem {
    rss_kb: u64,
    hwm_kb: u64,
    swap_kb: u64,
}

fn read_mem() -> Mem {
    let mut m = Mem::default();
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return m;
    };
    for line in status.lines() {
        let mut parts = line.split_whitespace();
        let (Some(key), Some(val)) = (parts.next(), parts.next()) else {
            continue;
        };
        let kb = val.parse().unwrap_or(0);
        match key {
            "VmRSS:" => m.rss_kb = kb,
            "VmHWM:" => m.hwm_kb = kb,
            "VmSwap:" => m.swap_kb = kb,
            _ => {}
        }
    }
    m
}

fn read_vram_mib() -> Option<u64> {
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=pid,used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let me = std::process::id().to_string();
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once(',').filter(|(pid, _)| pid.trim() == me))
            .filter_map(|(_, mb)| mb.trim().parse::<u64>().ok())
            .sum(),
    )
}

fn read_card_used_mib() -> Option<u64> {
    let out = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
        .flatten()
}

fn mib(kb: u64) -> f64 {
    kb as f64 / 1024.0
}

fn report(stage: &str, before: Mem, after: Mem, vram_before: Option<u64>, vram_after: Option<u64>) {
    let vram = match (vram_before, vram_after) {
        (Some(b), Some(a)) => format!("{:>7} MiB", a as i64 - b as i64),
        _ => "      —".to_string(),
    };
    println!(
        "  {stage:<34} {:>9.1} {:>9.1} {:>9.1} {vram}",
        mib(after.rss_kb) - mib(before.rss_kb),
        mib(after.hwm_kb),
        mib(after.swap_kb),
    );
}

#[tokio::main]
async fn main() {
    let url = std::env::var("DATABASE_URL").expect("Set DATABASE_URL to run mem_bench");

    println!("\n  ═══ cuba-memorys memory baseline ═══\n");
    println!(
        "  {:<34} {:>9} {:>9} {:>9} {:>11}",
        "stage", "ΔRSS MiB", "peak MiB", "swap MiB", "ΔVRAM"
    );
    println!("  {}", "─".repeat(76));

    let base = read_mem();
    let vram_base = read_vram_mib();
    let others_on_card = read_card_used_mib().unwrap_or(0) - vram_base.unwrap_or(0);
    println!(
        "  {:<34} {:>9.1} {:>9.1} {:>9.1} {:>7} MiB",
        "process start",
        mib(base.rss_kb),
        mib(base.hwm_kb),
        mib(base.swap_kb),
        vram_base.unwrap_or(0)
    );

    let (before, vb) = (read_mem(), read_vram_mib());
    let pool = memory_industry::db::create_pool(&url)
        .await
        .expect("connect to database");
    report("postgres pool", before, read_mem(), vb, read_vram_mib());

    let (before, vb) = (read_mem(), read_vram_mib());
    let started = Instant::now();
    let _ = memory_industry::embeddings::onnx::embed("warm up the embedder").await;
    let embed_load = started.elapsed();
    report(
        "embedder (first embed)",
        before,
        read_mem(),
        vb,
        read_vram_mib(),
    );

    let (before, vb) = (read_mem(), read_vram_mib());
    let started = Instant::now();
    let warmed = memory_industry::search::rerank::warm_up().await;
    let rerank_load = started.elapsed();
    report(
        "reranker (warm_up)",
        before,
        read_mem(),
        vb,
        read_vram_mib(),
    );

    let (before, vb) = (read_mem(), read_vram_mib());
    let started = Instant::now();
    let raw: Vec<(pgvector::Vector,)> = sqlx::query_as(
        "SELECT embedding FROM brain_observations
         WHERE embedding IS NOT NULL AND observation_type != 'superseded'
           AND trust = 'trusted'
         ORDER BY id LIMIT 5000",
    )
    .fetch_all(&pool)
    .await
    .expect("reading embeddings for the OOD fit");
    let n = raw.len();
    let embeddings: Vec<Vec<f32>> = raw.into_iter().map(|(v,)| v.to_vec()).collect();
    let d = embeddings.first().map(Vec::len).unwrap_or(0);
    let stats = memory_industry::search::ood::OodStats::fit(&embeddings);
    let ood_time = started.elapsed();
    report("OOD fit", before, read_mem(), vb, read_vram_mib());

    let peak = read_mem();
    println!("  {}", "─".repeat(76));
    println!(
        "\n  peak RSS {:.1} MiB · swap {:.1} MiB · VRAM {} MiB (this process; {} MiB held by others)",
        mib(peak.hwm_kb),
        mib(peak.swap_kb),
        read_vram_mib().unwrap_or(0),
        others_on_card,
    );
    println!(
        "  embedder load {:.2}s · reranker {} {:.2}s · OOD fit {:.2}s over n={n} d={d}",
        embed_load.as_secs_f64(),
        if warmed { "warm" } else { "absent," },
        rerank_load.as_secs_f64(),
        ood_time.as_secs_f64(),
    );
    if stats.is_none() && n > 0 {
        println!("  note: the OOD fit returned nothing — below MIN_SAMPLES_FOR_OOD?");
    }
    println!(
        "\n  A delta here means nothing unless the page cache, the GPU and the machine\n  \
         were in the same state as the run you compare it against.\n"
    );

    pool.close().await;
}
