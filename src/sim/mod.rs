//! Offline simulator for probe exploration / charting strategies.
//!
//! Runs pluggable strategies against a *frozen* galaxy snapshot
//! (`tests/fixtures/galaxy_*`) so competing algorithms can be compared
//! apples-to-apples, deterministically, without touching the live agent.
//!
//! Two scenarios (see the respective modules):
//! - [`scenario1`] — the gate network is unknown; reach a set of target systems
//!   with a fixed probe budget, minimizing summed reach-time. Online graph
//!   exploration: a system's gate connections are revealed only when a probe
//!   physically reaches it.
//! - `scenario2` (todo) — full information; chart every waypoint under the
//!   global API rate limit, maximizing charts/second.
//!
//! The simulator reuses the agent's real cost primitives so results can't drift
//! from in-game behaviour:
//! - jump cooldown `60 + round(dist)` seconds (`universe/pathfinding.rs`)
//! - [`crate::util::distance`] (Euclidean, `max(1, round)`)
//! - [`crate::util::estimated_travel_duration`] for intra-system navigation.

pub mod scenario1;

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::util::{Coord, distance};

/// Integer (x, y) point — a system or waypoint position. Implements [`Coord`]
/// so it can feed the agent's own [`crate::util::distance`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pt {
    pub x: i64,
    pub y: i64,
}

impl Coord for Pt {
    fn x(&self) -> i64 {
        self.x
    }
    fn y(&self) -> i64 {
        self.y
    }
}

/// Jump cooldown (seconds) between two gates `dist` apart — the agent's model
/// (`let cooldown = 60 + distance;` in `universe/pathfinding.rs`). This is the
/// edge weight used for all gate-graph routing.
pub fn jump_cooldown(dist: i64) -> i64 {
    60 + dist
}

// ---------------------------------------------------------------------------
// Universe fixture (`universe.json`) — systems + charted gate edges.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct UniverseSystem {
    pub symbol: String,
    pub x: i64,
    pub y: i64,
    #[serde(rename = "type")]
    pub system_type: String,
    pub has_gate: bool,
    pub gate_charted: bool,
    #[serde(default)]
    pub gate_under_construction: bool,
    #[serde(default)]
    pub p_t5: Option<f64>,
    #[serde(default)]
    pub faction_hqs: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UniverseFixture {
    systems: Vec<UniverseSystem>,
    edges: Vec<(String, String)>,
}

/// Processed galaxy: system lookup + ground-truth gate adjacency. The
/// simulator treats `adjacency` as the oracle; a strategy only "knows" an
/// entry once a probe has charted that system in the sim.
pub struct Galaxy {
    pub systems: HashMap<String, UniverseSystem>,
    /// Undirected gate adjacency among gate systems (ground truth).
    pub adjacency: HashMap<String, Vec<String>>,
}

impl Galaxy {
    pub fn load(path: impl AsRef<Path>) -> Self {
        let raw = fs::read_to_string(path.as_ref())
            .unwrap_or_else(|e| panic!("read {}: {e}", path.as_ref().display()));
        let fixture: UniverseFixture = serde_json::from_str(&raw).expect("parse universe.json");

        let systems: HashMap<String, UniverseSystem> = fixture
            .systems
            .into_iter()
            .map(|s| (s.symbol.clone(), s))
            .collect();

        let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
        for (a, b) in &fixture.edges {
            adjacency.entry(a.clone()).or_default().push(b.clone());
            adjacency.entry(b.clone()).or_default().push(a.clone());
        }
        for v in adjacency.values_mut() {
            v.sort();
            v.dedup();
        }

        Galaxy { systems, adjacency }
    }

    pub fn pos(&self, sym: &str) -> Pt {
        let s = &self.systems[sym];
        Pt { x: s.x, y: s.y }
    }

    /// Euclidean system distance via the agent's own formula.
    pub fn distance(&self, a: &str, b: &str) -> i64 {
        distance(&self.pos(a), &self.pos(b))
    }

    /// Jump cooldown (seconds) between two gate systems.
    pub fn jump_cooldown(&self, a: &str, b: &str) -> i64 {
        jump_cooldown(self.distance(a, b))
    }

    /// Ground-truth gate neighbours of a system (empty if uncharted/no gate).
    pub fn neighbors(&self, sym: &str) -> &[String] {
        self.adjacency.get(sym).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn gate_systems(&self) -> impl Iterator<Item = &UniverseSystem> {
        self.systems.values().filter(|s| s.has_gate)
    }
}

// ---------------------------------------------------------------------------
// Waypoint fixture (`waypoints_gatesystems.ndjson`) — per-system layouts.
// Consumed by scenario 2 (charting). Loaded here so both scenarios share types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct SystemWaypoints {
    pub system: String,
    pub x: i64,
    pub y: i64,
    /// `(symbol, type, x, y)` per waypoint (positions relative to the system).
    pub waypoints: Vec<(String, String, i64, i64)>,
}

/// Load the newline-delimited per-system waypoint layouts.
pub fn load_waypoints_ndjson(path: impl AsRef<Path>) -> Vec<SystemWaypoints> {
    let raw = fs::read_to_string(path.as_ref())
        .unwrap_or_else(|e| panic!("read {}: {e}", path.as_ref().display()));
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse waypoints ndjson line"))
        .collect()
}

/// Default fixture directory (relative to the crate root).
pub const FIXTURE_DIR: &str = "tests/fixtures/galaxy_2026-07-04";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_universe_fixture() {
        let g = Galaxy::load(format!("{FIXTURE_DIR}/universe.json"));
        assert!(g.systems.len() > 6000);
        assert!(g.gate_systems().count() > 3000);
        // adjacency is symmetric
        for (a, ns) in &g.adjacency {
            for b in ns {
                assert!(g.neighbors(b).iter().any(|x| x == a), "{a}<->{b} asym");
            }
        }
    }

    #[test]
    fn loads_waypoint_sample() {
        let w = load_waypoints_ndjson(format!("{FIXTURE_DIR}/waypoints_sample25.ndjson"));
        assert_eq!(w.len(), 25);
        assert!(w.iter().all(|s| !s.waypoints.is_empty()));
    }
}
