use super::Universe;
use crate::models::{SystemSymbol, WaypointSymbol};
use log::*;
use quadtree_rs::area::AreaBuilder;
use quadtree_rs::{Quadtree, point::Point};
use std::cmp::max;
use std::collections::BTreeMap;

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
    // Construct a map containing every jumpgate and its traversable connections
    pub async fn jumpgate_graph(&self) -> BTreeMap<WaypointSymbol, JumpGate> {
        // Start by loading the list of jumpgates from system list
        // Load the detailed system waypoints and the jumpgate connections if the waypoint is charted
        // This also tells us which jumpgates are constructed
        let jumpgates = self
            .systems()
            .into_iter()
            .filter(|s| !s.waypoints.is_empty())
            .filter_map(|s| {
                let filtered = s
                    .waypoints
                    .iter()
                    .filter(|w| w.waypoint_type == "JUMP_GATE")
                    .map(|w| w.symbol.clone())
                    .collect::<Vec<_>>();
                match filtered.len() {
                    0 => None,
                    1 => Some((s, filtered.first().unwrap().clone())),
                    _ => panic!("Multiple jumpgates in system {}", s.symbol),
                }
            })
            .collect::<Vec<_>>();
        let mut waypoints = BTreeMap::new();
        for (system, waypoint_symbol) in &jumpgates {
            let waypoint = self.detailed_waypoint(&waypoint_symbol).await;
            if !waypoint.is_uncharted() {
                let _gate = self.get_jumpgate_connections(&waypoint_symbol).await;
            }
            waypoints.insert(waypoint_symbol, (system, waypoint));
        }

        // Read connections from self.jumpgates (includes uncharted gates that we know the connections for)
        let mut graph: BTreeMap<WaypointSymbol, JumpGate> = BTreeMap::new();
        for (&waypoint_symbol, (_s, waypoint)) in waypoints.iter() {
            let known_connections = self.jumpgates.contains_key(&waypoint_symbol);
            let gate = JumpGate {
                active_connections: vec![],
                is_constructed: !waypoint.is_under_construction,
                all_connections_known: known_connections,
            };
            graph.insert(waypoint_symbol.clone(), gate);
        }
        for kv in self.jumpgates.iter() {
            let (src_symbol, jump_info) = kv.pair();
            let (src_system, src_waypoint) = waypoints.get(&src_symbol).unwrap();
            if src_waypoint.is_under_construction {
                continue;
            }
            for dst_symbol in &jump_info.connections {
                let (dst_system, dst_waypoint) = waypoints.get(&dst_symbol).unwrap();
                if dst_waypoint.is_under_construction {
                    continue;
                }
                let distance = src_system.distance(&dst_system);
                let cooldown = 60 + distance;

                // src -> dst
                let src_entry = graph.get_mut(src_symbol).unwrap();
                src_entry
                    .active_connections
                    .push((dst_symbol.clone(), cooldown));

                // for dst -> src, insert unless 'complete'
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
