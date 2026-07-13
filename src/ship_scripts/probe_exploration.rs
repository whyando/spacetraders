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
            // Pause exploration while the treasury is thin: charting jumps cost fuel, and
            // when credits are low we'd rather let the traders rebuild capital than keep
            // spending on exploration. Gated here at Init, a probe finishes its current
            // target (it only pauses before taking a new one — no mid-flight abort) and
            // idles until credits recover past the threshold.
            const MIN_CREDITS_FOR_EXPLORATION: i64 = 1_000_000;
            if ac.ctx.ledger.available_credits() < MIN_CREDITS_FOR_EXPLORATION {
                ship.set_state_description("Exploration paused: credits < 1M");
                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                return None; // stay in Init; re-check next tick
            }

            // The graph already excludes under-construction gates (their construction
            // status is known up front), so any reserved target is safe to jump to.
            let target = ac
                .get_probe_jumpgate_reservation(&ship.symbol(), &ship.waypoint())
                .await;
            match target {
                Some(target) => {
                    ship.set_state_description(&format!("Exploring jumpgate {}", target));
                    Some(Exploring(target))
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
            debug!(
                "Navigating to {} in {}s via path {}",
                target_jumpgate, duration, path_str
            );
            ship.set_state_description(&format!(
                "Navigating to {} in {}s",
                target_jumpgate, duration
            ));

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
