//! Sensor-array survey of a remote system.
//!
//! Run by an explorer (fast + sensor array + warp): route to the target system
//! and scan once to reveal every waypoint's traits (markets/shipyards), bypassing
//! their uncharted state. The revealed traits are ingested so the agent learns the
//! markets — the permanent intel probes (INTEL_SYSTEMS) then gather live prices and
//! ship listings. Guarded by a DB flag so each system is scanned a single time.

use crate::{config::CONFIG, models::SystemSymbol, ship_controller::ShipController};
use log::*;

use super::probe::goto_waypoint_anywhere;

// Entry point for a SHIP_EXPLORER assigned the Survey behaviour: scan the configured
// target system, then idle (it has done its job; awaiting future targets).
pub async fn run_scanner(ship: ShipController) {
    info!("Starting survey scanner for {}", ship.symbol());
    ship.wait_for_transit().await;

    if let Some(system) = CONFIG.survey_system.clone() {
        scan_system(&ship, &system).await;
    }

    loop {
        ship.set_state_description("Survey complete; idle");
        tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
    }
}

// Route to `system` and scan it once (no-op if already done).
async fn scan_system(ship: &ShipController, system: &SystemSymbol) {
    let done_key = format!("survey_done/{}", system);
    if ship.ctx.db.get_value::<bool>(&done_key).await.unwrap_or(false) {
        return;
    }
    info!("[{}] Surveying {}", ship.symbol(), system);

    // Route into the target system (jumping gate-to-gate) and land on its gate.
    let gate = ship.ctx.universe.get_jumpgate(system).await;
    ship.set_state_description(&format!("Surveying: en route to {}", system));
    goto_waypoint_anywhere(ship, &gate).await;

    // One scan reveals every in-range waypoint's traits; scan_waypoints ingests them so
    // the agent learns the markets/shipyards (which the intel probes then monitor).
    ship.set_state_description(&format!("Scanning {}", system));
    let scanned = ship.scan_waypoints().await;
    let markets = scanned.iter().filter(|w| w.is_market()).count();
    let shipyards = scanned.iter().filter(|w| w.is_shipyard()).count();
    info!(
        "[{}] Scan of {} revealed {} markets, {} shipyards of {} waypoints",
        ship.symbol(),
        system,
        markets,
        shipyards,
        scanned.len()
    );

    ship.ctx.db.set_value(&done_key, &true).await;
    info!("[{}] Survey of {} complete", ship.symbol(), system);
}
