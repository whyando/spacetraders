//! Scenario 1 — reach a set of target systems across an *unknown* gate network
//! with a fixed probe budget, minimizing summed reach-time.
//!
//! Model (grounded in the agent's mechanics):
//! - System coordinates are globally known; **gate connections are not** — a
//!   system's neighbours are revealed only when a probe charts it (arrives).
//! - A probe can jump from a charted system to any of its revealed neighbours;
//!   the edge weight is the cooldown `60 + round(dist)` seconds.
//! - All probes start co-located at `home` (its gate is charted at t=0).
//! - "Reaching" a target = a probe physically arrives at that system.
//!
//! The simulator is event-driven (min-heap over probe-free times). A probe
//! commits to a chosen *frontier* system (reachable-but-uncharted) and is busy
//! for the full multi-hop route there; on arrival it charts (revealing the
//! oracle's neighbours), which widens the frontier for everyone.
//!
//! Strategies plug in via [`Strategy`]; [`run_all`] scores several against the
//! same fixture + target set for apples-to-apples comparison.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};

use pathfinding::directed::dijkstra::dijkstra_all;

use super::{Galaxy, Pt, jump_cooldown};
use crate::util::distance;

/// Seconds an idle probe waits before re-polling for newly-opened frontier.
const IDLE_POLL: i64 = 60;
/// Safety bound on event iterations (fixture has < 7k systems).
const MAX_ITERS: u64 = 5_000_000;

/// A system we want to reach.
#[derive(Clone, Debug)]
pub struct Target {
    pub symbol: String,
    pub pos: Pt,
    /// Priority weight (e.g. `p_t5`); 1.0 = unweighted.
    pub weight: f64,
}

// ---------------------------------------------------------------------------
// Strategy interface
// ---------------------------------------------------------------------------

/// What a strategy is allowed to see when routing one free probe. It sees
/// global coordinates and the *revealed* frontier — never the oracle.
pub struct Observation<'a> {
    pub galaxy: &'a Galaxy,
    /// Which probe is being routed (for strategies that keep per-probe state).
    pub probe_id: usize,
    /// The probe's current location (a charted system).
    pub probe_loc: &'a str,
    /// Unreserved, reachable, uncharted systems this probe could go chart.
    pub frontier: &'a [String],
    /// Route cost (seconds) from the probe to each frontier system.
    pub dist_from_probe: &'a HashMap<String, i64>,
    /// Targets not yet reached.
    pub targets_remaining: &'a [Target],
}

impl Observation<'_> {
    fn pos(&self, sym: &str) -> Pt {
        self.galaxy.pos(sym)
    }
    /// Min Euclidean distance from `f` to any remaining target (0 if none left).
    fn nearest_target_dist(&self, f: &str) -> f64 {
        let p = self.pos(f);
        self.targets_remaining
            .iter()
            .map(|t| distance(&p, &t.pos) as f64)
            .fold(f64::INFINITY, f64::min)
            .min(f64::MAX)
    }
}

pub trait Strategy {
    fn name(&self) -> &str;
    /// Pick a frontier system for this probe, or `None` to idle.
    fn choose(&self, obs: &Observation) -> Option<String>;
}

/// Baseline: nearest unreserved uncharted frontier, target-agnostic. Mirrors
/// the production policy (`exploration.rs:get_probe_jumpgate_reservation`).
pub struct NearestFrontier;
impl Strategy for NearestFrontier {
    fn name(&self) -> &str {
        "nearest-frontier (baseline)"
    }
    fn choose(&self, obs: &Observation) -> Option<String> {
        obs.frontier
            .iter()
            .min_by_key(|f| obs.dist_from_probe[*f])
            .cloned()
    }
}

/// A*-style: prefer frontier gates that are cheap to reach **and** geometrically
/// advance toward the nearest remaining target. `lambda` trades charting cost
/// against progress-toward-target (seconds per unit distance).
pub struct GreedyToTarget {
    pub lambda: f64,
    label: String,
}
impl GreedyToTarget {
    pub fn new(lambda: f64) -> Self {
        Self {
            lambda,
            label: format!("greedy-to-target (λ={lambda})"),
        }
    }
}
impl Strategy for GreedyToTarget {
    fn name(&self) -> &str {
        &self.label
    }
    fn choose(&self, obs: &Observation) -> Option<String> {
        if obs.targets_remaining.is_empty() {
            return obs
                .frontier
                .iter()
                .min_by_key(|f| obs.dist_from_probe[*f])
                .cloned();
        }
        obs.frontier
            .iter()
            .min_by(|a, b| {
                let sa = obs.dist_from_probe[*a] as f64 + self.lambda * obs.nearest_target_dist(a);
                let sb = obs.dist_from_probe[*b] as f64 + self.lambda * obs.nearest_target_dist(b);
                sa.partial_cmp(&sb).unwrap()
            })
            .cloned()
    }
}

/// Beam-per-target: each probe **persistently commits** to one target and works
/// it until reached, then re-commits to the least-covered remaining target
/// nearest its current location. This spreads the fleet across the target field
/// (parallel beams) instead of thrashing between targets every decision. Within
/// a beam it A*'s toward the committed target (`g + λ·h`).
pub struct BeamPerTarget {
    pub lambda: f64,
    /// probe_id -> committed target symbol.
    assign: RefCell<HashMap<usize, String>>,
    label: String,
}
impl BeamPerTarget {
    pub fn new(lambda: f64) -> Self {
        Self {
            lambda,
            assign: RefCell::new(HashMap::new()),
            label: format!("beam-per-target (λ={lambda})"),
        }
    }

    /// The target this probe should pursue: keep its current commitment if that
    /// target is still unreached, else commit to the remaining target with the
    /// fewest probes on it (tie-break: nearest to this probe, then symbol).
    fn focus_for(&self, obs: &Observation) -> Target {
        let remaining: HashSet<&str> = obs
            .targets_remaining
            .iter()
            .map(|t| t.symbol.as_str())
            .collect();
        let mut assign = self.assign.borrow_mut();

        if let Some(cur) = assign.get(&obs.probe_id) {
            if remaining.contains(cur.as_str()) {
                let cur = cur.clone();
                return obs
                    .targets_remaining
                    .iter()
                    .find(|t| t.symbol == cur)
                    .unwrap()
                    .clone();
            }
        }

        // Reassign: load per still-remaining target, from other probes' commitments.
        let mut load: HashMap<&str, usize> = HashMap::new();
        for (pid, sym) in assign.iter() {
            if *pid != obs.probe_id && remaining.contains(sym.as_str()) {
                *load.entry(sym.as_str()).or_insert(0) += 1;
            }
        }
        let probe_pos = obs.galaxy.pos(obs.probe_loc);
        let pick = obs
            .targets_remaining
            .iter()
            .min_by(|a, b| {
                let la = *load.get(a.symbol.as_str()).unwrap_or(&0);
                let lb = *load.get(b.symbol.as_str()).unwrap_or(&0);
                la.cmp(&lb)
                    .then_with(|| {
                        let da = distance(&probe_pos, &a.pos);
                        let db = distance(&probe_pos, &b.pos);
                        da.cmp(&db)
                    })
                    .then_with(|| a.symbol.cmp(&b.symbol))
            })
            .unwrap()
            .clone();
        assign.insert(obs.probe_id, pick.symbol.clone());
        pick
    }
}
impl Strategy for BeamPerTarget {
    fn name(&self) -> &str {
        &self.label
    }
    fn choose(&self, obs: &Observation) -> Option<String> {
        if obs.targets_remaining.is_empty() {
            return obs
                .frontier
                .iter()
                .min_by_key(|f| obs.dist_from_probe[*f])
                .cloned();
        }
        let fp = self.focus_for(obs).pos;
        obs.frontier
            .iter()
            .min_by(|a, b| {
                let sa = obs.dist_from_probe[*a] as f64
                    + self.lambda * distance(&obs.pos(a), &fp) as f64;
                let sb = obs.dist_from_probe[*b] as f64
                    + self.lambda * distance(&obs.pos(b), &fp) as f64;
                sa.partial_cmp(&sb).unwrap()
            })
            .cloned()
    }
}

// ---------------------------------------------------------------------------
// Simulation
// ---------------------------------------------------------------------------

struct Probe {
    loc: String,
    /// Frontier system currently being travelled to (charted on arrival).
    reserved: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub strategy: String,
    pub num_probes: usize,
    pub total_targets: usize,
    pub reached: usize,
    /// Per-target reach time (seconds); `None` = never reached.
    pub reach_times: Vec<(String, Option<i64>)>,
    pub sum_reach: i64,
    pub mean_reach: f64,
    pub makespan: i64,
    /// Systems charted over the whole run (work done).
    pub charted: usize,
    pub sim_end: i64,
    /// Ordered charting events `(time, system, is_target)` — for visualization.
    pub events: Vec<(i64, String, bool)>,
}

/// Run one strategy against the galaxy from `home`, for `num_probes` probes.
pub fn simulate(
    galaxy: &Galaxy,
    home: &str,
    targets: &[Target],
    num_probes: usize,
    strategy: &dyn Strategy,
) -> RunResult {
    let mut charted: HashSet<String> = HashSet::new();
    charted.insert(home.to_string());
    let mut known_adj: HashMap<String, Vec<String>> = HashMap::new();
    known_adj.insert(home.to_string(), galaxy.neighbors(home).to_vec());

    let mut reserved: HashSet<String> = HashSet::new();
    let mut probes: Vec<Probe> = (0..num_probes)
        .map(|_| Probe {
            loc: home.to_string(),
            reserved: None,
        })
        .collect();

    let target_set: HashSet<&str> = targets.iter().map(|t| t.symbol.as_str()).collect();
    let mut reach: HashMap<String, i64> = HashMap::new();
    let mut events: Vec<(i64, String, bool)> = Vec::new();
    // Targets co-located with home (already charted) count as reached at t=0.
    for t in targets {
        if charted.contains(&t.symbol) {
            reach.insert(t.symbol.clone(), 0);
        }
    }

    let mut heap: BinaryHeap<Reverse<(i64, u64, usize)>> = BinaryHeap::new();
    let mut seq: u64 = 0;
    for pid in 0..num_probes {
        heap.push(Reverse((0, seq, pid)));
        seq += 1;
    }
    let mut n_traveling: usize = 0;
    let mut now: i64 = 0;
    let mut iters: u64 = 0;

    while let Some(Reverse((t, _, pid))) = heap.pop() {
        iters += 1;
        if iters > MAX_ITERS {
            break;
        }
        now = t;

        // 1. Arrival effects: probe reaches its reserved frontier and charts it.
        if let Some(dest) = probes[pid].reserved.take() {
            n_traveling -= 1;
            probes[pid].loc = dest.clone();
            if charted.insert(dest.clone()) {
                known_adj.insert(dest.clone(), galaxy.neighbors(&dest).to_vec());
                events.push((t, dest.clone(), target_set.contains(dest.as_str())));
            }
            reserved.remove(&dest);
            if target_set.contains(dest.as_str()) {
                reach.entry(dest).or_insert(t);
            }
        }

        if reach.len() == target_set.len() {
            break; // all targets reached
        }

        // 2. Assign next frontier for this probe.
        let start = probes[pid].loc.clone();
        let succ = |n: &String| -> Vec<(String, i64)> {
            if !charted.contains(n) {
                return Vec::new(); // frontier nodes are terminal
            }
            known_adj
                .get(n)
                .map(|ns| {
                    ns.iter()
                        .map(|m| (m.clone(), jump_cooldown(galaxy.distance(n, m))))
                        .collect()
                })
                .unwrap_or_default()
        };
        let dmap = dijkstra_all(&start, succ);

        let mut dist_from_probe: HashMap<String, i64> = HashMap::new();
        for (n, (_, c)) in &dmap {
            if !charted.contains(n) && !reserved.contains(n) {
                dist_from_probe.insert(n.clone(), *c);
            }
        }

        if dist_from_probe.is_empty() {
            // Nothing reachable for this probe right now.
            if n_traveling == 0 {
                continue; // no traveller will open new frontier -> stay idle
            }
            heap.push(Reverse((t + IDLE_POLL, seq, pid)));
            seq += 1;
            continue;
        }

        let frontier: Vec<String> = dist_from_probe.keys().cloned().collect();
        let remaining: Vec<Target> = targets
            .iter()
            .filter(|t| !reach.contains_key(&t.symbol))
            .cloned()
            .collect();
        let obs = Observation {
            galaxy,
            probe_id: pid,
            probe_loc: &start,
            frontier: &frontier,
            dist_from_probe: &dist_from_probe,
            targets_remaining: &remaining,
        };

        match strategy.choose(&obs) {
            Some(f) => {
                let cost = dist_from_probe[&f];
                reserved.insert(f.clone());
                probes[pid].reserved = Some(f);
                n_traveling += 1;
                heap.push(Reverse((t + cost, seq, pid)));
                seq += 1;
            }
            None => {
                if n_traveling == 0 {
                    continue;
                }
                heap.push(Reverse((t + IDLE_POLL, seq, pid)));
                seq += 1;
            }
        }
    }

    // Assemble result.
    let mut reach_times: Vec<(String, Option<i64>)> = targets
        .iter()
        .map(|t| (t.symbol.clone(), reach.get(&t.symbol).copied()))
        .collect();
    reach_times.sort_by_key(|(s, _)| s.clone());
    let reached_vals: Vec<i64> = reach.values().copied().collect();
    let reached = reached_vals.len();
    let sum_reach: i64 = reached_vals.iter().sum();
    let mean_reach = if reached > 0 {
        sum_reach as f64 / reached as f64
    } else {
        0.0
    };
    let makespan = reached_vals.iter().copied().max().unwrap_or(0);

    RunResult {
        strategy: strategy.name().to_string(),
        num_probes,
        total_targets: target_set.len(),
        reached,
        reach_times,
        sum_reach,
        mean_reach,
        makespan,
        charted: charted.len(),
        sim_end: now,
        events,
    }
}

/// Run several strategies against the same setup and return their results.
pub fn run_all(
    galaxy: &Galaxy,
    home: &str,
    targets: &[Target],
    num_probes: usize,
    strategies: &[&dyn Strategy],
) -> Vec<RunResult> {
    strategies
        .iter()
        .map(|s| simulate(galaxy, home, targets, num_probes, *s))
        .collect()
}

// ---------------------------------------------------------------------------
// Setup helpers (deterministic, so A/B runs are reproducible).
// ---------------------------------------------------------------------------

/// Systems reachable from `home` over the ground-truth gate graph (BFS).
/// These are the only systems any strategy could ever reach.
pub fn reachable_from(galaxy: &Galaxy, home: &str) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut q = VecDeque::new();
    seen.insert(home.to_string());
    q.push_back(home.to_string());
    while let Some(s) = q.pop_front() {
        for n in galaxy.neighbors(&s) {
            if seen.insert(n.clone()) {
                q.push_back(n.clone());
            }
        }
    }
    seen
}

/// Pick a home system: prefer a faction HQ with a gate, else the highest-degree
/// charted gate system. Deterministic.
pub fn pick_home(galaxy: &Galaxy) -> String {
    if let Some(hq) = galaxy
        .gate_systems()
        .filter(|s| !s.faction_hqs.is_empty() && s.gate_charted)
        .min_by(|a, b| a.symbol.cmp(&b.symbol))
    {
        return hq.symbol.clone();
    }
    galaxy
        .systems
        .values()
        .filter(|s| s.has_gate && s.gate_charted)
        .max_by(|a, b| {
            galaxy
                .neighbors(&a.symbol)
                .len()
                .cmp(&galaxy.neighbors(&b.symbol).len())
                .then(b.symbol.cmp(&a.symbol))
        })
        .map(|s| s.symbol.clone())
        .expect("no charted gate system for home")
}

pub enum TargetMode {
    /// Farthest reachable gate systems by ground-truth jump-cost from home.
    Farthest,
    /// Reachable T5 gate systems: `p_t5 >= min_p` (0.5 = the agent's real
    /// "T5 system" threshold).
    T5 { min_p: f64 },
    /// Reachable faction-capital (HQ) gate systems.
    Capitals,
    /// Union of T5 systems and capitals — the agent's real high-value targets.
    T5AndCapitals { min_p: f64 },
}

/// Select deterministic, reachable target systems. `n == 0` means "all that
/// match" (used for the T5/capital modes); otherwise take the top `n`.
pub fn select_targets(galaxy: &Galaxy, home: &str, n: usize, mode: TargetMode) -> Vec<Target> {
    let reach = reachable_from(galaxy, home);
    // Ground-truth jump-cost distance from home (for Farthest / tie-breaks).
    let succ = |s: &String| -> Vec<(String, i64)> {
        galaxy
            .neighbors(s)
            .iter()
            .map(|m| (m.clone(), jump_cooldown(galaxy.distance(s, m))))
            .collect()
    };
    let dmap = dijkstra_all(&home.to_string(), succ);

    let is_reachable_gate =
        |s: &&super::UniverseSystem| s.symbol != home && s.has_gate && reach.contains(&s.symbol);

    let mut cand: Vec<&super::UniverseSystem> = galaxy
        .systems
        .values()
        .filter(is_reachable_gate)
        .filter(|s| match &mode {
            TargetMode::Farthest => true,
            TargetMode::T5 { min_p } => s.p_t5.unwrap_or(0.0) >= *min_p,
            TargetMode::Capitals => !s.faction_hqs.is_empty(),
            TargetMode::T5AndCapitals { min_p } => {
                s.p_t5.unwrap_or(0.0) >= *min_p || !s.faction_hqs.is_empty()
            }
        })
        .collect();

    match mode {
        TargetMode::Farthest => cand.sort_by(|a, b| {
            let da = dmap.get(&a.symbol).map(|(_, c)| *c).unwrap_or(i64::MAX);
            let db = dmap.get(&b.symbol).map(|(_, c)| *c).unwrap_or(i64::MAX);
            db.cmp(&da).then(a.symbol.cmp(&b.symbol))
        }),
        // High-value first, deterministic by symbol.
        _ => cand.sort_by(|a, b| {
            let pa = a.p_t5.unwrap_or(0.0);
            let pb = b.p_t5.unwrap_or(0.0);
            pb.partial_cmp(&pa).unwrap().then(a.symbol.cmp(&b.symbol))
        }),
    }

    let take = if n == 0 {
        cand.len()
    } else {
        n.min(cand.len())
    };
    cand.into_iter()
        .take(take)
        .map(|s| Target {
            symbol: s.symbol.clone(),
            pos: Pt { x: s.x, y: s.y },
            weight: s.p_t5.unwrap_or(1.0).max(0.01),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::FIXTURE_DIR;

    #[test]
    fn beam_reaches_all_reachable_targets() {
        let g = Galaxy::load(format!("{FIXTURE_DIR}/universe.json"));
        let home = pick_home(&g);
        let targets = select_targets(&g, &home, 8, TargetMode::Farthest);
        assert!(!targets.is_empty());
        let strat = BeamPerTarget::new(1.0);
        let r = simulate(&g, &home, &targets, 20, &strat);
        // Targets were chosen from home's reachable set, so all must be reached.
        assert_eq!(r.reached, targets.len(), "unreached: {:?}", r.reach_times);
        assert!(r.sum_reach > 0);
    }
}
