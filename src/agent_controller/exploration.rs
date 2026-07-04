use super::context::AgentContext;
use crate::models::{SystemSymbol, WaypointSymbol};
use dashmap::DashMap;
use pathfinding::directed::dijkstra::dijkstra_all;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Clone)]
pub struct ExplorationManager {
    ctx: Arc<AgentContext>,
    probe_jumpgate_reservations: Arc<DashMap<String, WaypointSymbol>>,
    /// Beam-per-target: each charting probe's committed important-system target,
    /// kept until that target's gate is charted (in-memory; re-derives on restart).
    probe_target_systems: Arc<DashMap<String, SystemSymbol>>,
    explorer_reservations: Arc<DashMap<String, SystemSymbol>>,
    t5_system_reservations: Arc<DashMap<String, SystemSymbol>>,
    probe_reserve_mutex_guard: Arc<tokio::sync::Mutex<()>>,
    explorer_reserve_mutex_guard: Arc<tokio::sync::Mutex<()>>,
    t5_reserve_mutex_guard: Arc<tokio::sync::Mutex<()>>,
}

impl ExplorationManager {
    pub fn new(
        ctx: Arc<AgentContext>,
        probe_jumpgate_reservations: DashMap<String, WaypointSymbol>,
        probe_target_systems: DashMap<String, SystemSymbol>,
        explorer_reservations: DashMap<String, SystemSymbol>,
        t5_system_reservations: DashMap<String, SystemSymbol>,
    ) -> Self {
        Self {
            ctx,
            probe_jumpgate_reservations: Arc::new(probe_jumpgate_reservations),
            probe_target_systems: Arc::new(probe_target_systems),
            explorer_reservations: Arc::new(explorer_reservations),
            t5_system_reservations: Arc::new(t5_system_reservations),
            probe_reserve_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
            explorer_reserve_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
            t5_reserve_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn get_probe_jumpgate_reservation(
        &self,
        ship_symbol: &str,
        ship_loc: &WaypointSymbol,
    ) -> Option<WaypointSymbol> {
        let existing = self.probe_jumpgate_reservations.get(ship_symbol);
        if let Some(existing) = existing {
            return Some(existing.value().clone());
        }

        let _lock = self.probe_reserve_mutex_guard.lock().await;
        let start = self.ctx.universe.get_jumpgate(&ship_loc.system()).await;
        // The home gate's own connections seed the entire frontier, but nothing
        // charts them up front: a probe only charts gates it *reaches*, and it can't
        // reach anything until the home gate has outgoing edges in the graph. Chart it
        // on demand here (under the reserve lock, so only the first probe does the
        // work). get_jumpgate_connections also invalidates the cached graph, so an
        // idle probe picks up targets the moment the home gate finishes construction.
        if !self.ctx.universe.connections_known(&start) {
            self.ctx.universe.get_jumpgate_connections(&start).await;
        }
        let graph = self.ctx.universe.jumpgate_graph().await;
        let reachables = dijkstra_all(&start, |node| {
            graph.get(node).unwrap().active_connections.clone()
        });
        // Target-directed exploration (beam-per-target). The probes exist to open
        // routes to the systems that matter — high-P(T5) systems and faction
        // capitals (all have gates). Each probe *commits* to one such target and
        // keeps charting toward it until that target's gate is charted, then
        // re-commits to the least-covered remaining target (load-balanced across
        // the fleet). Within its beam it picks the reachable, uncharted, unreserved
        // frontier gate minimising  g + λ·h : g = jump-cooldown cost to reach it
        // (Dijkstra from this probe), h = Euclidean distance from its system to the
        // committed target. With no important targets left, h = 0 and this degrades
        // to the old nearest-frontier policy (so the rest of the network is mapped).
        const LAMBDA: f64 = 1.0;
        const T5_THRESHOLD: f64 = 0.5;

        let capitals: HashSet<String> = self
            .ctx
            .universe
            .get_factions()
            .into_iter()
            .filter_map(|f| f.headquarters)
            .map(|s| s.to_string())
            .collect();
        let mut coords: HashMap<String, (i64, i64)> = HashMap::new();
        let mut targets: Vec<(SystemSymbol, (i64, i64))> = Vec::new();
        for sys in self.ctx.universe.systems() {
            let sym = sys.symbol.to_string();
            let important = sys.p_t5().map(|p| p >= T5_THRESHOLD) == Some(true)
                || capitals.contains(&sym);
            if important {
                if let Some(gate) =
                    sys.waypoints.iter().find(|w| w.waypoint_type == "JUMP_GATE")
                {
                    if !self.ctx.universe.connections_known(&gate.symbol) {
                        targets.push((sys.symbol.clone(), (sys.x, sys.y)));
                    }
                }
            }
            coords.insert(sym, (sys.x, sys.y));
        }

        // Commit this probe to one target: keep its current commitment if that
        // target is still uncharted, else take the least-covered remaining target
        // (tie-break: nearest to the probe, then symbol). None once all charted.
        let target_syms: HashSet<String> = targets.iter().map(|(s, _)| s.to_string()).collect();
        let probe_pos = coords
            .get(&ship_loc.system().to_string())
            .copied()
            .unwrap_or((0, 0));
        let sq_dist = |a: (i64, i64), b: (i64, i64)| -> f64 {
            let (dx, dy) = ((a.0 - b.0) as f64, (a.1 - b.1) as f64);
            dx * dx + dy * dy
        };
        let keep = self
            .probe_target_systems
            .get(ship_symbol)
            .map(|e| e.value().clone())
            .filter(|s| target_syms.contains(&s.to_string()));
        let focus_sym: Option<SystemSymbol> = keep.or_else(|| {
            let mut load: HashMap<String, usize> = HashMap::new();
            for e in self.probe_target_systems.iter() {
                if e.key().as_str() != ship_symbol {
                    let v = e.value().to_string();
                    if target_syms.contains(&v) {
                        *load.entry(v).or_insert(0) += 1;
                    }
                }
            }
            targets
                .iter()
                .min_by(|(sa, pa), (sb, pb)| {
                    let la = *load.get(&sa.to_string()).unwrap_or(&0);
                    let lb = *load.get(&sb.to_string()).unwrap_or(&0);
                    la.cmp(&lb)
                        .then_with(|| {
                            sq_dist(probe_pos, *pa)
                                .partial_cmp(&sq_dist(probe_pos, *pb))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .then_with(|| sa.to_string().cmp(&sb.to_string()))
                })
                .map(|(s, _)| s.clone())
        });
        if let Some(focus) = &focus_sym {
            let changed = self
                .probe_target_systems
                .get(ship_symbol)
                .map(|e| e.value() != focus)
                .unwrap_or(true);
            if changed {
                self.probe_target_systems
                    .insert(ship_symbol.to_string(), focus.clone());
                self.ctx
                    .db
                    .save_probe_target_systems(&self.ctx.callsign, &self.probe_target_systems)
                    .await;
            }
        }
        let focus_pos: Option<(i64, i64)> = focus_sym
            .as_ref()
            .and_then(|s| coords.get(&s.to_string()).copied());

        let heuristic = |gate: &WaypointSymbol| -> f64 {
            let Some(tpos) = focus_pos else {
                return 0.0;
            };
            let Some(&gpos) = coords.get(&gate.system().to_string()) else {
                return 0.0;
            };
            sq_dist(gpos, tpos).sqrt()
        };

        let mut candidates: Vec<(WaypointSymbol, i64)> = Vec::new();
        for (gate, (_pre, d)) in &reachables {
            if graph.get(gate).unwrap().all_connections_known {
                continue;
            }
            if self
                .probe_jumpgate_reservations
                .iter()
                .any(|x| x.value() == gate)
            {
                continue;
            }
            candidates.push((gate.clone(), *d));
        }
        let target = candidates
            .into_iter()
            .min_by(|(ga, da), (gb, db)| {
                let sa = *da as f64 + LAMBDA * heuristic(ga);
                let sb = *db as f64 + LAMBDA * heuristic(gb);
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(gate, _)| gate);
        match target {
            Some(target) => {
                self.probe_jumpgate_reservations
                    .insert(ship_symbol.to_string(), target.clone());
                self.ctx
                    .db
                    .save_probe_jumpgate_reservations(
                        &self.ctx.callsign,
                        &self.probe_jumpgate_reservations,
                    )
                    .await;
                Some(target)
            }
            None => None,
        }
    }

    pub async fn clear_probe_jumpgate_reservation(&self, ship_symbol: &str) {
        {
            let target = self.probe_jumpgate_reservations.get(ship_symbol).unwrap();
            assert!(self.ctx.universe.connections_known(target.value()));
        }
        self.probe_jumpgate_reservations.remove(ship_symbol);
        self.ctx
            .db
            .save_probe_jumpgate_reservations(
                &self.ctx.callsign,
                &self.probe_jumpgate_reservations,
            )
            .await;
    }

    pub async fn get_explorer_reservation(
        &self,
        ship_symbol: &str,
        ship_loc: &SystemSymbol,
    ) -> Option<SystemSymbol> {
        let existing = self.explorer_reservations.get(ship_symbol);
        if let Some(existing) = existing {
            return Some(existing.value().clone());
        }

        let _lock = self.explorer_reserve_mutex_guard.lock().await;
        let graph = self.ctx.universe.warp_jump_graph().await;
        let reachables = dijkstra_all(ship_loc, |node| {
            graph
                .get(node)
                .unwrap()
                .iter()
                .map(|(s, d)| (s.clone(), d.duration))
        });
        let mut starter_systems = vec![];
        for system in self.ctx.universe.systems() {
            if !system.is_starter_system() {
                continue;
            }
            let system_symbol = system.symbol.clone();
            if system_symbol == *ship_loc {
                starter_systems.push((system_symbol.clone(), &0));
            }
            if let Some((_pre, cd)) = reachables.get(&system_symbol) {
                starter_systems.push((system_symbol, cd));
            }
        }
        starter_systems.sort_by_key(|(_system, d)| *d);

        let target = starter_systems.iter().find(|(system, _d)| {
            let reserved = self
                .explorer_reservations
                .iter()
                .any(|x| x.value() == system);
            !reserved
        });

        match target {
            Some((target, _)) => {
                self.explorer_reservations
                    .insert(ship_symbol.to_string(), target.clone());
                self.ctx
                    .db
                    .save_explorer_reservations(&self.ctx.callsign, &self.explorer_reservations)
                    .await;
                Some(target.clone())
            }
            None => None,
        }
    }

    // Reserve the nearest (by jumpgate hops from our home gate) unreserved system
    // with P(T5) >= 0.5, so each t5 trader works a distinct high-value system.
    pub async fn get_t5_system_reservation(&self, ship_symbol: &str) -> Option<SystemSymbol> {
        let existing = self.t5_system_reservations.get(ship_symbol);
        if let Some(existing) = existing {
            return Some(existing.value().clone());
        }

        let _lock = self.t5_reserve_mutex_guard.lock().await;
        let home_gate = self
            .ctx
            .universe
            .get_jumpgate_opt(&self.ctx.starting_system())
            .await?;
        let candidates = self
            .ctx
            .universe
            .reachable_high_t5_systems(&home_gate)
            .await;

        let target = candidates.into_iter().find(|system| {
            !self
                .t5_system_reservations
                .iter()
                .any(|x| x.value() == system)
        });

        match target {
            Some(target) => {
                self.t5_system_reservations
                    .insert(ship_symbol.to_string(), target.clone());
                self.ctx
                    .db
                    .save_t5_system_reservations(&self.ctx.callsign, &self.t5_system_reservations)
                    .await;
                Some(target)
            }
            None => None,
        }
    }
}
