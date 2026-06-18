use super::{JumpGateInfo, Universe};
use crate::api_client::api_models::WaypointDetailed;
use crate::models::{ShipFlightMode, System, SystemSymbol, WaypointSymbol};
use crate::util;
use log::*;
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
        // Gate nodes come straight from the (bulk-loaded) system list — one gate per
        // system, keyed to its system for distance math. We deliberately do NOT fetch
        // per-waypoint details here (that would be a galaxy-wide API storm). Only
        // *charted* gates (in self.jumpgates) carry connection/construction data;
        // every other gate is treated as an uncharted, presumed-constructed frontier
        // node that a probe resolves on arrival.
        let mut gate_systems: BTreeMap<WaypointSymbol, System> = BTreeMap::new();
        for system in self.systems() {
            let mut gates = system
                .waypoints
                .iter()
                .filter(|w| w.waypoint_type == "JUMP_GATE")
                .map(|w| w.symbol.clone());
            if let Some(gate) = gates.next() {
                assert!(gates.next().is_none(), "Multiple jumpgates in {}", system.symbol);
                gate_systems.insert(gate, system);
            }
        }

        // Charted gates' connection/construction info, read from the in-memory cache.
        // Snapshot first (can't hold a DashMap ref across the heal's insert), then
        // re-derive only gates still flagged not-constructed (self-heals one that has
        // since finished building).
        let snapshot: Vec<(WaypointSymbol, JumpGateInfo)> = self
            .jumpgates
            .iter()
            .map(|kv| (kv.key().clone(), kv.value().clone()))
            .collect();
        let mut gate_info: BTreeMap<WaypointSymbol, JumpGateInfo> = BTreeMap::new();
        for (symbol, info) in snapshot {
            let info = if info.is_constructed {
                info
            } else {
                self.get_jumpgate_connections(&symbol).await
            };
            gate_info.insert(symbol, info);
        }

        // Nodes
        let mut graph: BTreeMap<WaypointSymbol, JumpGate> = BTreeMap::new();
        for symbol in gate_systems.keys() {
            let info = gate_info.get(symbol);
            graph.insert(
                symbol.clone(),
                JumpGate {
                    active_connections: vec![],
                    // uncharted gates are presumed constructed so probes will visit them
                    is_constructed: info.map(|g| g.is_constructed).unwrap_or(true),
                    all_connections_known: info.is_some(),
                },
            );
        }

        // Edges from charted gates' connection lists
        for (src_symbol, jump_info) in &gate_info {
            if !jump_info.is_constructed {
                continue;
            }
            let Some(src_system) = gate_systems.get(src_symbol) else {
                continue;
            };
            for dst_symbol in &jump_info.connections {
                // skip links to gates we know are still under construction
                if gate_info.get(dst_symbol).is_some_and(|g| !g.is_constructed) {
                    continue;
                }
                let Some(dst_system) = gate_systems.get(dst_symbol) else {
                    continue;
                };
                let distance = src_system.distance(dst_system);
                let cooldown = 60 + distance;

                // src -> dst
                graph
                    .get_mut(src_symbol)
                    .unwrap()
                    .active_connections
                    .push((dst_symbol.clone(), cooldown));

                // for dst -> src, insert unless dst's connections are already known
                let entry = graph.get_mut(dst_symbol).unwrap();
                if !entry.all_connections_known {
                    entry
                        .active_connections
                        .push((src_symbol.clone(), cooldown));
                }
            }
        }

        graph
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
