use crate::{
    agent_controller::AgentController,
    database::DbClient,
    models::{LogisticsScriptConfig, PlanLength, PlannerConfig, ShipFlightMode, SystemSymbol},
    ship_controller::ShipController,
    universe::pathfinding::EdgeType,
};
use ExplorerState::*;
use chrono::Duration;
use log::*;
use pathfinding::directed::dijkstra::dijkstra;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum ExplorerState {
    Init,
    Navigating(SystemSymbol),
    Trading(SystemSymbol),
    Exit,
}

pub async fn run_explorer(ship: ShipController, _db: DbClient, ac: AgentController) {
    info!("Starting script explorer for {}", ship.symbol());
    ship.wait_for_transit().await;

    let mut state = Init;

    while state != Exit {
        let next_state = tick(&ship, &state, &ac).await;
        if let Some(next_state) = next_state {
            state = next_state;
        }
        if let Trading(_) = state {
            break;
        }
    }

    if let Trading(system) = state {
        assert_eq!(ship.system(), system);
        info!("Explorer trading in target system {}", system);
        ship.set_state_description(&format!("Trading in {}", system));

        let task_manager = ac.task_manager.clone();
        // let waypoints = ship.universe.get_system_waypoints(&system).await;
        // let inner_market_waypoints = market_waypoints(&waypoints, Some(200));
        let config = LogisticsScriptConfig {
            use_planner: true,
            planner_config: Some(PlannerConfig {
                plan_length: PlanLength::Ramping(
                    Duration::seconds(30),
                    Duration::minutes(10),
                    1.85,
                ),
                max_compute_time: Duration::seconds(5),
            }),
            // waypoint_allowlist: Some(inner_market_waypoints.clone()),
            waypoint_allowlist: None,
            allow_shipbuying: false,
            allow_market_refresh: true,
            allow_construction: false,
            min_profit: 5000,
        };
        crate::ship_scripts::logistics::run(ship.clone(), task_manager, config, ac).await;
    }
}

async fn tick(
    ship: &ShipController,
    state: &ExplorerState,
    ac: &AgentController,
) -> Option<ExplorerState> {
    match state {
        Init => {
            let target = ac
                .get_explorer_reservation(&ship.symbol(), &ship.system())
                .await;
            let desc = match &target {
                Some(target) => format!("Navigating to {}", target),
                None => "No target".to_string(),
            };
            ship.set_state_description(&desc);
            match target {
                Some(target) => Some(Navigating(target)),
                None => Some(Exit),
            }
        }
        Navigating(target) => {
            if &ship.system() == target {
                // might need to empty cargo before starting trading state
                return Some(Trading(target.clone()));
            }

            // Plan route
            let graph = ship.ctx.universe.warp_jump_graph().await;
            let start = ship.system();
            let (path, duration) = dijkstra(
                &start,
                |node| {
                    graph
                        .get(node)
                        .unwrap()
                        .iter()
                        .map(|(s, d)| (s.clone(), d.duration))
                },
                |node| node == target,
            )
            .expect("No path to target");

            let path_str = path
                .windows(2)
                .map(|pair| {
                    let s = &pair[0];
                    let t = &pair[1];
                    let edge = &graph[s][t];
                    let type_ = match edge.edge_type {
                        EdgeType::Jumpgate => "JUMP",
                        EdgeType::Warp => "WARP",
                    };
                    format!("{} {} -> {}", type_, s, t)
                })
                .collect::<Vec<_>>()
                .join(", ");
            let desc = format!(
                "Navigating to {} in {}s via path {}",
                target, duration, path_str
            );
            debug!("{}", desc);
            ship.set_state_description(&desc);

            // Execute route
            for pair in path.windows(2) {
                let s = &pair[0];
                let t = &pair[1];
                let edge = &graph[s][t];
                match edge.edge_type {
                    EdgeType::Jumpgate => {
                        let src_gate = ship.ctx.universe.get_jumpgate(s).await;
                        let dst_gate = ship.ctx.universe.get_jumpgate(t).await;
                        ship.goto_waypoint(&src_gate).await;
                        ship.jump(&dst_gate).await;
                    }
                    EdgeType::Warp => {
                        let waypoint = ship.ctx.universe.waypoint(&ship.waypoint());
                        if waypoint.is_market() {
                            ship.refuel(ship.fuel_capacity(), false).await;
                            ship.full_load_cargo("FUEL").await;
                        } else {
                            let required_fuel = edge.fuel;
                            ship.refuel(required_fuel, true).await;
                        }

                        if ship.current_fuel() < edge.fuel {
                            info!("Not enough fuel to warp to {}", t);
                            return Some(Exit);
                        }

                        // target waypoint:
                        // if jumpgate in target system: warp to jumpgate
                        // otherwise: warp to any waypoint in target system
                        let warp_target = match ship.ctx.universe.get_jumpgate_opt(t).await {
                            Some(jumpgate) => jumpgate,
                            None => ship.ctx.universe.first_waypoint(t).await,
                        };
                        ship.warp(ShipFlightMode::Cruise, &warp_target).await;
                    }
                }
            }

            // might need to empty cargo before starting trading state
            Some(Trading(target.clone()))
        }
        Trading(_system) => {
            panic!("Invalid state");
        }
        Exit => {
            panic!("Invalid state");
        }
    }
}
