use crate::{
    agent_controller::AgentController, models::WaypointSymbol, ship_controller::ShipController,
};
use ExplorerState::*;
use log::*;
use pathfinding::directed::dijkstra::dijkstra;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// How long an idle probe waits before re-checking for newly-charted targets.
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum ExplorerState {
    Init,
    Exploring(WaypointSymbol),
    Idle,
}

pub async fn run_jumpgate_probe(ship: ShipController, ac: AgentController) {
    info!("Starting script jumpgate probe for {}", ship.symbol());
    ship.wait_for_transit().await;

    // Never exit: the reachable-uncharted frontier grows as other probes chart
    // gates, so an idle probe polls and resumes when new targets appear.
    let mut state = Init;
    loop {
        let next_state = tick(&ship, &state, &ac).await;
        if let Some(next_state) = next_state {
            state = next_state;
        }
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
                .get_probe_jumpgate_reservation(&ship.symbol(), &ship.waypoint())
                .await;
            match target {
                Some(target) => {
                    // A gate that's still under construction can't be jumped to (the
                    // API rejects it). Exclude it and pick another target rather than
                    // letting the jump panic and crash the agent.
                    let construction = ship.ctx.universe.get_construction(&target).await;
                    let under_construction = construction
                        .data
                        .as_ref()
                        .map(|c| !c.is_complete)
                        .unwrap_or(false);
                    if under_construction {
                        info!("Jumpgate {} is under construction; skipping", target);
                        ship.ctx
                            .universe
                            .mark_jumpgate_under_construction(&target)
                            .await;
                        ac.clear_probe_jumpgate_reservation(&ship.symbol()).await;
                        Some(Init)
                    } else {
                        ship.set_state_description(&format!("Exploring jumpgate {}", target));
                        Some(Exploring(target))
                    }
                }
                None => Some(Idle),
            }
        }
        Exploring(target_jumpgate) => {
            let start_jumpgate = ship.ctx.universe.get_jumpgate(&ship.system()).await;

            let graph = ship.ctx.universe.jumpgate_graph().await;
            let (path, duration) = dijkstra(
                &start_jumpgate,
                |node| graph.get(node).unwrap().active_connections.clone(),
                |node| node == target_jumpgate,
            )
            .expect("No path to target jumpgate");
            let path_str = path
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(" -> ");
            let desc = format!(
                "Navigating to {} in {}s via path {}",
                target_jumpgate, duration, path_str
            );
            debug!("{}", desc);
            ship.set_state_description(&desc);

            // Execute route
            ship.goto_waypoint(&start_jumpgate).await;
            for gate in path.iter().skip(1) {
                ship.jump(gate).await;
            }
            assert_eq!(ship.waypoint(), *target_jumpgate);
            let _connections = ship
                .ctx
                .universe
                .get_jumpgate_connections(target_jumpgate)
                .await;

            ac.clear_probe_jumpgate_reservation(&ship.symbol()).await;
            Some(Init)
        }
        Idle => {
            // Stage at the home jump gate (no-op if we're already sitting on a gate
            // from the last charting run) so we can jump the moment a target opens up,
            // then wait before re-checking the frontier.
            let start_jumpgate = ship.ctx.universe.get_jumpgate(&ship.system()).await;
            if ship.waypoint() != start_jumpgate {
                ship.goto_waypoint(&start_jumpgate).await;
            }
            ship.set_state_description("Idle at jumpgate, waiting for new targets");
            tokio::time::sleep(IDLE_POLL_INTERVAL).await;
            Some(Init)
        }
    }
}
