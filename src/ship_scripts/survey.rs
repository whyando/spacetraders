//! One-time sensor-array survey of a remote system.
//!
//! The command frigate is our fastest ship (ION_DRIVE_II) and carries a Sensor
//! Array, so it's the right tool to bootstrap intel on a far, uncharted system:
//! route in, scan once to reveal every waypoint's traits (markets/shipyards),
//! snapshot those for live prices + ship listings, then head home and resume
//! trading. Guarded by a DB flag so it runs a single time per system.

use crate::{
    models::{SystemSymbol, WaypointSymbol},
    ship_controller::ShipController,
};
use log::*;

use super::probe::goto_waypoint_anywhere;

pub async fn run_survey(ship: &ShipController, system: &SystemSymbol) {
    let db = &ship.ctx.db;
    let done_key = format!("survey_done/{}", system);
    if db.get_value::<bool>(&done_key).await.unwrap_or(false) {
        return;
    }
    info!("[{}] Starting survey of {}", ship.symbol(), system);

    // Route into the target system (jumping gate-to-gate as needed) and land on its gate.
    let gate = ship.ctx.universe.get_jumpgate(system).await;
    ship.set_state_description(&format!("Surveying: en route to {}", system));
    goto_waypoint_anywhere(ship, &gate).await;

    // One sensor-array scan reveals every in-range waypoint's traits, bypassing their
    // uncharted state; scan_waypoints ingests them so the agent learns the markets.
    ship.set_state_description(&format!("Surveying: scanning {}", system));
    let scanned = ship.scan_waypoints().await;
    let mut targets: Vec<WaypointSymbol> = scanned
        .iter()
        .filter(|w| w.is_market() || w.is_shipyard())
        .map(|w| w.symbol.clone())
        .collect();
    info!(
        "[{}] Scan of {} revealed {} market/shipyard waypoints of {} total",
        ship.symbol(),
        system,
        targets.len(),
        scanned.len()
    );
    // Flag the survey done now that the scan (the critical, agent-learns-the-markets
    // step) succeeded — so a hiccup in the snapshot below doesn't trigger a re-scan.
    db.set_value(&done_key, &true).await;

    // Snapshot: visit each market/shipyard for a first read of live prices + ship listings.
    // The permanent intel probes take over continuous monitoring afterwards.
    targets.sort();
    for wp in &targets {
        ship.set_state_description(&format!("Surveying market/shipyard {}", wp));
        goto_waypoint_anywhere(ship, wp).await;
        let w = ship.ctx.universe.detailed_waypoint(wp).await;
        if w.is_market() {
            ship.refresh_market().await;
        }
        if w.is_shipyard() {
            ship.refresh_shipyard().await;
        }
    }

    info!("[{}] Survey of {} complete", ship.symbol(), system);

    // Head back to the home system so the ship can resume its normal (home) duties.
    let home_gate = ship
        .ctx
        .universe
        .get_jumpgate(&ship.ctx.starting_system())
        .await;
    ship.set_state_description("Surveying: returning home");
    goto_waypoint_anywhere(ship, &home_gate).await;
}
