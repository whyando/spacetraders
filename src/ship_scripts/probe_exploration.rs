use crate::{models::WaypointSymbol, ship_controller::ShipController};
use ExplorerState::*;
use log::*;
use pathfinding::directed::dijkstra::dijkstra;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum ExplorerState {
    Init,
    Exploring(WaypointSymbol),
    Exit,
}

pub async fn run_jumpgate_probe(ship: ShipController) {
    info!("Starting script jumpgate probe for {}", ship.symbol());
    ship.wait_for_transit().await;

    let mut state = Init;

    while state != Exit {
        let next_state = tick(&ship, &state).await;
        if let Some(next_state) = next_state {
            state = next_state;
        }
    }
}

async fn tick(ship: &ShipController, state: &ExplorerState) -> Option<ExplorerState> {
    match state {
        Init => {
            // Could be existing reservation, or a new one
            let target = ship
                .agent_controller
                .get_probe_jumpgate_reservation(&ship.symbol(), &ship.waypoint())
                .await;
            let desc = match &target {
                Some(target) => format!("Exploring jumpgate {}", target),
                None => "No target".to_string(),
            };
            ship.set_state_description(&desc);
            match target {
                Some(target) => Some(Exploring(target)),
                None => Some(Exit),
            }
        }
        Exploring(target_jumpgate) => {
            let start_jumpgate = ship.universe.get_jumpgate(&ship.system()).await;

            // Plan route
            let graph = ship.universe.jumpgate_graph().await;
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
                ship.jump(&gate).await;
            }
            // Get connections
            assert_eq!(ship.waypoint(), *target_jumpgate);
            let _connections = ship
                .universe
                .get_jumpgate_connections(&target_jumpgate)
                .await;

            ship.agent_controller
                .clear_probe_jumpgate_reservation(&ship.symbol())
                .await;
            Some(Init)
        }
        Exit => {
            panic!("Invalid state");
        }
    }
}
