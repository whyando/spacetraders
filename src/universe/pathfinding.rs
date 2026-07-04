use super::Universe;
use crate::api_client::api_models::WaypointDetailed;
use crate::models::{ShipFlightMode, System, SystemSymbol, WaypointSymbol};
use crate::util;
use log::*;
use pathfinding::directed::dijkstra::dijkstra_all;
use quadtree_rs::area::AreaBuilder;
use quadtree_rs::{Quadtree, point::Point};
use std::cmp::max;
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct NavEdge {
    pub flight_mode: ShipFlightMode,
    pub fuel_cost: i64,
    pub distance: i64,
    pub duration: i64,
}

pub struct JumpGate {
    pub active_connections: Vec<(WaypointSymbol, i64)>,
    pub is_constructed: bool,
    pub all_connections_known: bool,
}

#[derive(Debug, Clone)]
pub enum EdgeType {
    Warp,
    Jumpgate,
}

#[derive(Debug, Clone)]
pub struct WarpEdge {
    pub duration: i64,
    pub edge_type: EdgeType,
    pub fuel: i64,
}

impl Universe {
    // Cached map of every jumpgate and its traversable connections.
    // `get_jumpgate_connections` invalidates this whenever a gate's connections
    // change, so a newly-charted gate immediately opens up the frontier for the
    // other probes.
    pub async fn jumpgate_graph(&self) -> Arc<BTreeMap<WaypointSymbol, JumpGate>> {
        // Needs the full galaxy: wait for the one-time background load.
        self.await_systems_loaded().await;
        self.jumpgate_graph
            .get_with((), async { Arc::new(self.build_jumpgate_graph().await) })
            .await
    }

    async fn build_jumpgate_graph(&self) -> BTreeMap<WaypointSymbol, JumpGate> {
        // One gate per system, keyed to its system for distance math. Gate systems'
        // waypoint details (incl. is_under_construction) are pre-fetched during the
        // galaxy load, so construction status is known here without per-build fetches.
        let mut gate_systems: BTreeMap<WaypointSymbol, System> = BTreeMap::new();
        for system in self.systems() {
            let mut gates = system
                .waypoints
                .iter()
                .filter(|w| w.waypoint_type == "JUMP_GATE")
                .map(|w| w.symbol.clone());
            if let Some(gate) = gates.next() {
                assert!(
                    gates.next().is_none(),
                    "Multiple jumpgates in {}",
                    system.symbol
                );
                gate_systems.insert(gate, system);
            }
        }

        // Construction status per gate. A waypoint flagged *constructed* is always
        // trustworthy (a gate never reads constructed while still building, and never
        // reverts). A waypoint flagged *under construction* can be stale after the gate
        // completes, so confirm those few against the construction site.
        let mut constructed: BTreeMap<WaypointSymbol, bool> = BTreeMap::new();
        for (gate, system) in &gate_systems {
            let flag = system
                .waypoints
                .iter()
                .find(|w| &w.symbol == gate)
                .and_then(|w| w.details.as_ref())
                .map(|d| d.is_under_construction);
            let is_constructed = match flag {
                Some(true) => {
                    let c = self.get_construction(gate).await;
                    c.data.as_ref().map(|c| c.is_complete).unwrap_or(true)
                }
                // constructed, or details not loaded yet -> presume constructed
                Some(false) | None => true,
            };
            constructed.insert(gate.clone(), is_constructed);
        }

        // Charted gates' connection lists (in-memory cache snapshot).
        let charted: BTreeMap<WaypointSymbol, Vec<WaypointSymbol>> = self
            .jumpgates
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().connections.clone()))
            .collect();

        // Nodes
        let mut graph: BTreeMap<WaypointSymbol, JumpGate> = BTreeMap::new();
        for gate in gate_systems.keys() {
            graph.insert(
                gate.clone(),
                JumpGate {
                    active_connections: vec![],
                    is_constructed: *constructed.get(gate).unwrap_or(&true),
                    all_connections_known: charted.contains_key(gate),
                },
            );
        }

        // Edges from charted, constructed gates. Under-construction gates are excluded
        // entirely (no edge in or out), so a route never jumps to one — at the endpoint
        // or mid-path. Constructed gates are safe to traverse through.
        for (src, connections) in &charted {
            if !constructed.get(src).copied().unwrap_or(true) {
                continue;
            }
            let Some(src_system) = gate_systems.get(src) else {
                continue;
            };
            for dst in connections {
                if !constructed.get(dst).copied().unwrap_or(true) {
                    continue;
                }
                let Some(dst_system) = gate_systems.get(dst) else {
                    continue;
                };
                let distance = src_system.distance(dst_system);
                let cooldown = 60 + distance;

                // src -> dst
                graph
                    .get_mut(src)
                    .unwrap()
                    .active_connections
                    .push((dst.clone(), cooldown));
                // dst -> src, unless dst is itself charted (it emits its own edges).
                let dst_node = graph.get_mut(dst).unwrap();
                if !dst_node.all_connections_known {
                    dst_node.active_connections.push((src.clone(), cooldown));
                }
            }
        }

        graph
    }

    // Is `target_gate` reachable from `from_gate` over the constructed + charted
    // jumpgate network (the same network ships actually jump across)?
    pub async fn is_jumpgate_reachable(
        &self,
        from_gate: &WaypointSymbol,
        target_gate: &WaypointSymbol,
    ) -> bool {
        if from_gate == target_gate {
            return true;
        }
        let graph = self.jumpgate_graph().await;
        let reachables = dijkstra_all(from_gate, |node| {
            graph
                .get(node)
                .map(|g| g.active_connections.clone())
                .unwrap_or_default()
        });
        reachables.contains_key(target_gate)
    }

    // Systems with P(T5) >= 0.5 whose jump gate is reachable from `from_gate`
    // over the constructed + charted jumpgate network, ordered nearest-first by
    // hop cost. A high-T5 system with no gate, or one not yet wired into the
    // network, is omitted — it surfaces here the moment its gate becomes
    // reachable, which is what gates explorer purchasing.
    pub async fn reachable_high_t5_systems(&self, from_gate: &WaypointSymbol) -> Vec<SystemSymbol> {
        const T5_THRESHOLD: f64 = 0.5;
        let graph = self.jumpgate_graph().await;
        let reachables = dijkstra_all(from_gate, |node| {
            graph
                .get(node)
                .map(|g| g.active_connections.clone())
                .unwrap_or_default()
        });

        let mut candidates: Vec<(SystemSymbol, i64)> = vec![];
        for system in self.systems() {
            if system.p_t5().map(|p| p >= T5_THRESHOLD) != Some(true) {
                continue;
            }
            let Some(gate) = system
                .waypoints
                .iter()
                .find(|w| w.waypoint_type == "JUMP_GATE")
                .map(|w| w.symbol.clone())
            else {
                continue;
            };
            let dist = if &gate == from_gate {
                Some(0)
            } else {
                reachables.get(&gate).map(|(_pre, d)| *d)
            };
            if let Some(dist) = dist {
                candidates.push((system.symbol.clone(), dist));
            }
        }
        candidates.sort_by_key(|(_system, d)| *d);
        candidates.into_iter().map(|(system, _d)| system).collect()
    }

    pub async fn warp_jump_graph(
        &self,
    ) -> BTreeMap<SystemSymbol, BTreeMap<SystemSymbol, WarpEdge>> {
        self.warp_jump_graph
            .get_with((), async {
                const EXPLORER_FUEL_CAPACITY: i64 = 800;
                const EXPLORER_SPEED: i64 = 30;
                self._warp_jump_graph(EXPLORER_FUEL_CAPACITY, EXPLORER_SPEED)
                    .await
            })
            .await
    }

    // Construct a map containing every system and its traversable connections
    pub async fn _warp_jump_graph(
        &self,
        warp_range: i64,
        engine_speed: i64,
    ) -> BTreeMap<SystemSymbol, BTreeMap<SystemSymbol, WarpEdge>> {
        let jumpgate_graph = self.jumpgate_graph().await;

        let systems = self
            .systems()
            .into_iter()
            .filter(|s| !s.waypoints.is_empty())
            .map(|s| {
                let filtered = s
                    .waypoints
                    .iter()
                    .filter(|w| w.waypoint_type == "JUMP_GATE")
                    .map(|w| w.symbol.clone())
                    .collect::<Vec<_>>();
                let jumpgate = match filtered.len() {
                    0 => None,
                    1 => Some(filtered.first().unwrap().clone()),
                    _ => panic!("Multiple jumpgates in system {}", s.symbol),
                };
                (s.symbol, s.x, s.y, jumpgate)
            })
            .collect::<Vec<_>>();

        info!("Constructing quadtree");
        let mut qt = Quadtree::<i64, SystemSymbol>::new_with_anchor(
            Point {
                // 2^18 = 262144
                x: -262144,
                y: -262144,
            },
            19,
        );
        for (symbol, x, y, _jumpgate) in systems.iter() {
            qt.insert_pt(Point { x: *x, y: *y }, symbol.clone());
        }
        info!("Constructing quadtree done");

        // Construct graph
        let mut warp_graph: BTreeMap<SystemSymbol, BTreeMap<SystemSymbol, WarpEdge>> =
            BTreeMap::new();
        for (symbol, x, y, jumpgate) in systems.iter() {
            let mut edges: BTreeMap<SystemSymbol, WarpEdge> = BTreeMap::new();

            // Add warp edges
            let neighbours = qt.query(
                AreaBuilder::default()
                    .anchor(Point {
                        x: x - warp_range,
                        y: y - warp_range,
                    })
                    .dimensions((2 * warp_range + 1, 2 * warp_range + 1))
                    .build()
                    .unwrap(),
            );
            for pt in neighbours {
                let coords = pt.anchor();
                let distance: i64 = {
                    let distance2 = (x - coords.x).pow(2) + (y - coords.y).pow(2);
                    max(1, (distance2 as f64).sqrt().round() as i64)
                };
                let duration =
                    (15f64 + (distance as f64) * 50f64 / (engine_speed as f64)).round() as i64;
                if distance <= warp_range {
                    edges.insert(
                        pt.value_ref().clone(),
                        WarpEdge {
                            duration,
                            edge_type: EdgeType::Warp,
                            fuel: distance,
                        },
                    );
                }
            }

            // Add jumpgate edges (overwrites warp edges if edge already exists)
            if let Some(jumpgate) = jumpgate {
                for conn in jumpgate_graph
                    .get(jumpgate)
                    .unwrap()
                    .active_connections
                    .iter()
                {
                    let (dest_symbol, cooldown) = conn;
                    edges.insert(
                        dest_symbol.system(),
                        WarpEdge {
                            duration: *cooldown,
                            edge_type: EdgeType::Jumpgate,
                            fuel: 0,
                        },
                    );
                }
            }
            warp_graph.insert(symbol.clone(), edges);
        }

        warp_graph
    }
}

// Returns a matrix between market waypoints. Assumes we can refuel at any waypoint.
// Weights are the travel duration in seconds between two waypoints
// Preferring BURN flight mode, and only CRUISE if the fuel capacity isn't high enough
pub fn market_adjacency_edges(
    market_waypoints: &[WaypointDetailed],
    ship_max_fuel: i64,
    ship_speed: i64,
) -> Vec<BTreeMap<usize, NavEdge>> {
    let mut edges = Vec::new();
    for w1 in market_waypoints.iter() {
        let mut row = BTreeMap::new();
        for (j, w2) in market_waypoints.iter().enumerate() {
            let dist = util::distance(w1, w2);
            let burn_fuel = util::fuel_cost(&ShipFlightMode::Burn, dist);
            let cruise_fuel = util::fuel_cost(&ShipFlightMode::Cruise, dist);
            if burn_fuel <= ship_max_fuel {
                row.insert(
                    j,
                    NavEdge {
                        flight_mode: ShipFlightMode::Burn,
                        fuel_cost: burn_fuel,
                        distance: dist,
                        duration: util::estimated_travel_duration(
                            &ShipFlightMode::Burn,
                            ship_speed,
                            dist,
                        ),
                    },
                );
            } else if cruise_fuel <= ship_max_fuel {
                row.insert(
                    j,
                    NavEdge {
                        flight_mode: ShipFlightMode::Cruise,
                        fuel_cost: cruise_fuel,
                        distance: dist,
                        duration: util::estimated_travel_duration(
                            &ShipFlightMode::Cruise,
                            ship_speed,
                            dist,
                        ),
                    },
                );
            }
        }
        edges.push(row);
    }
    assert_eq!(edges.len(), market_waypoints.len());
    edges
}

pub fn full_travel_matrix(
    market_waypoints: &[WaypointDetailed],
    ship_max_fuel: i64,
    ship_speed: i64,
) -> (Vec<Vec<f64>>, Vec<Vec<f64>>) {
    let mut durations: Vec<Vec<f64>> =
        vec![vec![0.; market_waypoints.len()]; market_waypoints.len()];
    let mut distances: Vec<Vec<f64>> =
        vec![vec![0.; market_waypoints.len()]; market_waypoints.len()];
    let edges = market_adjacency_edges(market_waypoints, ship_max_fuel, ship_speed);
    for i in 0..market_waypoints.len() {
        for j in 0..market_waypoints.len() {
            if i == j {
                durations[i][j] = 0.;
                distances[i][j] = 0.;
            } else if let Some(edge) = edges[i].get(&j) {
                durations[i][j] = edge.duration as f64;
                distances[i][j] = edge.distance as f64;
            } else {
                durations[i][j] = f64::INFINITY;
                distances[i][j] = f64::INFINITY;
            }
        }
    }

    // Fill in the rest of the matrix with floyd warshall
    for k in 0..market_waypoints.len() {
        for i in 0..market_waypoints.len() {
            for j in 0..market_waypoints.len() {
                durations[i][j] = durations[i][j].min(durations[i][k] + durations[k][j]);
                distances[i][j] = distances[i][j].min(distances[i][k] + distances[k][j]);
            }
        }
    }
    (durations, distances)
}
