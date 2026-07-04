//! Offline exploration/charting simulator.
//!
//! Runs pluggable strategies against a frozen galaxy snapshot and prints a
//! comparison table. See `src/sim/`.
//!
//! Usage:
//!   cargo run --release --bin explore_sim -- [--home SYM] [--targets N]
//!       [--probes N] [--mode farthest|value] [--lambda L]

use st::sim::scenario1::{
    self, BeamPerTarget, GreedyToTarget, NearestFrontier, RunResult, Strategy, TargetMode,
};
use st::sim::{FIXTURE_DIR, Galaxy};

fn arg<T: std::str::FromStr>(args: &[String], flag: &str, default: T) -> T {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n_probes: usize = arg(&args, "--probes", 20);
    let lambda: f64 = arg(&args, "--lambda", 1.0);
    let min_p: f64 = arg(&args, "--min-p5", 0.5);
    let mode_str = arg(&args, "--mode", "t5+capitals".to_string());
    let mode = match mode_str.as_str() {
        "t5" => TargetMode::T5 { min_p },
        "capitals" => TargetMode::Capitals,
        "farthest" => TargetMode::Farthest,
        _ => TargetMode::T5AndCapitals { min_p },
    };
    // T5/capital modes default to "all matching" (n=0); farthest needs a cap.
    let default_n = if mode_str == "farthest" { 12 } else { 0 };
    let n_targets: usize = arg(&args, "--targets", default_n);

    let galaxy = Galaxy::load(format!("{FIXTURE_DIR}/universe.json"));
    // The live agent's real starting home system (HQ X1-MV17-A1, faction UNITED).
    let home = args
        .iter()
        .position(|a| a == "--home")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "X1-MV17".to_string());

    let reachable = scenario1::reachable_from(&galaxy, &home);
    let targets = scenario1::select_targets(&galaxy, &home, n_targets, mode);

    println!(
        "Galaxy: {} systems, {} gate systems",
        galaxy.systems.len(),
        galaxy.gate_systems().count()
    );
    println!("Home:   {home}  ({} systems reachable)", reachable.len());
    println!("Mode:   {mode_str} (min_p5={min_p})");
    println!("Probes: {n_probes}   Targets: {}", targets.len());
    let preview: Vec<&str> = targets.iter().take(12).map(|t| t.symbol.as_str()).collect();
    let more = targets.len().saturating_sub(preview.len());
    println!(
        "Targets: {}{}",
        preview.join(", "),
        if more > 0 {
            format!(", … (+{more})")
        } else {
            String::new()
        }
    );
    println!();

    // Emit charting-event timelines (baseline / greedy / beam) for the animator.
    if let Some(i) = args.iter().position(|a| a == "--emit") {
        let path = args
            .get(i + 1)
            .cloned()
            .unwrap_or_else(|| "events.json".into());
        let strats: Vec<Box<dyn Strategy>> = vec![
            Box::new(NearestFrontier),
            Box::new(GreedyToTarget::new(1.0)),
            Box::new(BeamPerTarget::new(1.0)),
        ];
        let sarr: Vec<_> = strats
            .iter()
            .map(|s| {
                let r = scenario1::simulate(&galaxy, &home, &targets, n_probes, s.as_ref());
                serde_json::json!({
                    "name": r.strategy,
                    "sim_end": r.sim_end,
                    "makespan": r.makespan,
                    "reached": r.reached,
                    "charted": r.charted,
                    "events": r.events.iter()
                        .map(|(t, sym, is_t)| serde_json::json!([t, sym, is_t]))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let bundle = serde_json::json!({
            "home": home,
            "targets": targets.iter().map(|t| t.symbol.clone()).collect::<Vec<_>>(),
            "strategies": sarr,
        });
        std::fs::write(&path, serde_json::to_string(&bundle).unwrap()).unwrap();
        println!(
            "wrote {path}: {} strategies, {} targets",
            strats.len(),
            targets.len()
        );
        return;
    }

    let sweep = args.iter().any(|a| a == "--sweep");
    let mut owned: Vec<Box<dyn Strategy>> = vec![Box::new(NearestFrontier)];
    if sweep {
        // λ=0 must reproduce the baseline; higher λ = more target-directed.
        for l in [0.0, 0.5, 1.0, 2.0, 4.0] {
            owned.push(Box::new(GreedyToTarget::new(l)));
        }
        owned.push(Box::new(BeamPerTarget::new(1.0)));
    } else {
        owned.push(Box::new(GreedyToTarget::new(lambda)));
        owned.push(Box::new(BeamPerTarget::new(lambda)));
    }
    let refs: Vec<&dyn Strategy> = owned.iter().map(|b| b.as_ref()).collect();

    let results = scenario1::run_all(&galaxy, &home, &targets, n_probes, &refs);
    print_table(&results);
}

fn print_table(results: &[RunResult]) {
    println!(
        "{:<30} {:>8} {:>12} {:>12} {:>12} {:>10}",
        "strategy", "reached", "sum_reach", "mean_reach", "makespan", "charted"
    );
    println!("{}", "-".repeat(88));
    for r in results {
        println!(
            "{:<30} {:>8} {:>12} {:>12.0} {:>12} {:>10}",
            r.strategy,
            format!("{}/{}", r.reached, r.total_targets),
            r.sum_reach,
            r.mean_reach,
            r.makespan,
            r.charted,
        );
    }
    println!("\n(times in seconds; lower sum_reach/mean/makespan is better)");
}
