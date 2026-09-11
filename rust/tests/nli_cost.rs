use memory_industry::cognitive::nli;

#[tokio::test]
#[ignore = "medición, no aserción"]
async fn cost_by_premise_length() {
    assert!(
        nli::available() && nli::enabled(),
        "NLI model required by the local merge gate — install with cuba-memorys models nli"
    );
    let claim = "cuba-memorys está escrito en Java";
    let unit = "cuba-memorys es un servidor MCP de memoria escrito en Rust. ";

    let _ = nli::entails(unit, claim).await;

    eprintln!("\n chars | frases |    tiempo | veredicto");
    eprintln!("-------|--------|-----------|-----------");
    for n in [1usize, 2, 4, 8, 16, 32] {
        let premise = unit.repeat(n);
        let t0 = std::time::Instant::now();
        let v = nli::entails(&premise, claim).await.expect("veredicto");
        let dt = t0.elapsed();
        eprintln!(
            "{:6} | {:6} | {:>9?} | {}",
            premise.len(),
            n,
            dt,
            v.label.as_verdict()
        );
    }

    let premise = unit.repeat(6);
    let t0 = std::time::Instant::now();
    let futs: Vec<_> = (0..10).map(|_| nli::entails(&premise, claim)).collect();
    let _ = futures::future::join_all(futs).await;
    eprintln!(
        "\n10 evidencias concurrentes (como verify): {:?}\n",
        t0.elapsed()
    );
}
