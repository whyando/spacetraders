//!
//! Scrap script for ships
//!
//! Navigate to closest shipyard and scrap the ship
//!

use crate::ship_controller::ShipController;
use log::*;

pub async fn run(ship: ShipController) {
    info!("Starting script scrap for {}", ship.symbol());
    ship.wait_for_transit().await;

    let system_symbol = ship.system();
    let waypoints = ship.ctx.universe.get_system_waypoints(&system_symbol).await;
    let shipyards = ship
        .ctx
        .universe
        .get_system_shipyards_remote(&system_symbol)
        .await;

    let current_waypoint = waypoints
        .iter()
        .find(|w| w.symbol == ship.waypoint())
        .unwrap();
    let shipyard = shipyards.iter().min_by_key(|s| {
        let w = waypoints.iter().find(|w| w.symbol == s.symbol).unwrap();
        (current_waypoint.x - w.x).pow(2) + (current_waypoint.y - w.y).pow(2)
    });
    let shipyard = match shipyard {
        Some(s) => s,
        None => {
            info!("No shipyard in system. Failed to scrap {}", ship.symbol());
            return;
        }
    };

    ship.set_state_description(&format!("Scrapping ship at {}", shipyard.symbol));
    ship.goto_waypoint(&shipyard.symbol).await;
    ship.scrap().await;
}
