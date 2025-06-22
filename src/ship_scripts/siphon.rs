use crate::{
    database::DbClient, models::WaypointSymbol, ship_controller::ShipController,
    universe::WaypointFilter,
};
use SiphonShuttleState::*;
use lazy_static::lazy_static;
use log::*;
use serde::{Deserialize, Serialize};

lazy_static! {
    // The goods that can be siphoned from a gas giant
    static ref SIPHON_YIELDS: Vec<String> = vec![
        "LIQUID_NITROGEN".to_string(),
        "LIQUID_HYDROGEN".to_string(),
        "HYDROCARBON".to_string(),
    ];
}

async fn siphon_location(ship: &ShipController) -> WaypointSymbol {
    let waypoints = ship
        .universe
        .search_waypoints(&ship.system(), &vec![WaypointFilter::GasGiant])
        .await;
    assert!(waypoints.len() == 1);
    waypoints[0].symbol.clone()
}

async fn sell_location(ship: &ShipController) -> WaypointSymbol {
    let filters = SIPHON_YIELDS
        .iter()
        .map(|good| WaypointFilter::Exchanges(good.to_string()))
        .collect::<Vec<WaypointFilter>>();
    let waypoints = ship
        .universe
        .search_waypoints(&ship.system(), &filters)
        .await;
    assert!(waypoints.len() == 1);
    waypoints[0].symbol.clone()
}

pub async fn run_drone(ship: ShipController) {
    info!("Starting script siphon_drone for {}", ship.symbol());
    ship.wait_for_transit().await;

    let siphon_location = siphon_location(&ship).await;
    ship.goto_waypoint(&siphon_location).await;

    loop {
        let should_siphon = ship.cargo_space_available() > 0;
        if should_siphon {
            ship.siphon().await;
        } else {
            // transfer goods to shuttle, and wait till completed
            debug!("Siphon drone transfer initiated");
            ship.transfer_cargo().await;
            debug!("Siphon drone transfer completed");
        }
    }
    // info!("Finished script for {}", ship.symbol());
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum SiphonShuttleState {
    Loading,
    Selling,
}

pub async fn run_shuttle(ship: ShipController, db: DbClient) {
    info!("Starting script siphon_shuttle for {}", ship.symbol());
    ship.wait_for_transit().await;

    let siphon_location = siphon_location(&ship).await;
    let sell_location = sell_location(&ship).await;

    let key = format!("siphon_shuttle_state/{}", ship.symbol());
    let mut state: SiphonShuttleState = db.get_value(&key).await.unwrap_or(Loading);

    loop {
        match state {
            Loading => {
                if ship.cargo_space_available() == 0 {
                    state = Selling;
                    db.set_value(&key, &state).await;
                    continue;
                }
                ship.goto_waypoint(&siphon_location).await;
                ship.orbit().await;
                ship.receive_cargo().await;
            }
            Selling => {
                if ship.cargo_empty() {
                    state = Loading;
                    db.set_value(&key, &state).await;
                    continue;
                }
                ship.goto_waypoint(&sell_location).await;
                ship.sell_all_cargo().await;
            }
        }
    }
    // info!("Finished script for {}", ship.symbol());
}
