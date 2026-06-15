use super::context::AgentContext;
use crate::models::{SystemSymbol, WaypointSymbol};
use dashmap::DashMap;
use pathfinding::directed::dijkstra::dijkstra_all;
use std::sync::Arc;

#[derive(Clone)]
pub struct ExplorationManager {
    ctx: Arc<AgentContext>,
    probe_jumpgate_reservations: Arc<DashMap<String, WaypointSymbol>>,
    explorer_reservations: Arc<DashMap<String, SystemSymbol>>,
    probe_reserve_mutex_guard: Arc<tokio::sync::Mutex<()>>,
    explorer_reserve_mutex_guard: Arc<tokio::sync::Mutex<()>>,
}

impl ExplorationManager {
    pub fn new(
        ctx: Arc<AgentContext>,
        probe_jumpgate_reservations: DashMap<String, WaypointSymbol>,
        explorer_reservations: DashMap<String, SystemSymbol>,
    ) -> Self {
        Self {
            ctx,
            probe_jumpgate_reservations: Arc::new(probe_jumpgate_reservations),
            explorer_reservations: Arc::new(explorer_reservations),
            probe_reserve_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
            explorer_reserve_mutex_guard: Arc::new(tokio::sync::Mutex::new(())),
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
        let graph = self.ctx.universe.jumpgate_graph().await;
        let reachables = dijkstra_all(&start, |node| {
            graph.get(node).unwrap().active_connections.clone()
        });
        let mut reachable_gates = Vec::new();
        for (system, distance) in &reachables {
            reachable_gates.push((system.clone(), distance));
        }
        reachable_gates.sort_by_key(|(_gate, (_pre, d))| *d);
        let target = reachable_gates.iter().find(|(gate, (_pre, _d))| {
            let is_charted = graph.get(gate).unwrap().all_connections_known;
            if is_charted {
                return false;
            }
            let reserved = self
                .probe_jumpgate_reservations
                .iter()
                .any(|x| x.value() == gate);
            !reserved
        });
        match target {
            Some((target, _)) => {
                self.probe_jumpgate_reservations
                    .insert(ship_symbol.to_string(), target.clone());
                self.ctx
                    .db
                    .save_probe_jumpgate_reservations(
                        &self.ctx.callsign,
                        &self.probe_jumpgate_reservations,
                    )
                    .await;
                Some(target.clone())
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
}
