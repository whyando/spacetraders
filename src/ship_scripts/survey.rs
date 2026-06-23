//! Sensor-array survey of a remote system, then trade it.
//!
//! Run by an explorer (fast + sensor array + warp): route to the target system,
//! scan once to reveal every waypoint's traits (markets/shipyards) — which the
//! permanent intel probes then monitor — and then settle into trading that system
//! with the logistics planner, the same way the command frigate trades at home.

use std::sync::Arc;

use chrono::Duration;

use crate::{
    agent_controller::AgentController,
    config::CONFIG,
    models::{LogisticsScriptConfig, PlanLength, PlannerConfig, SystemSymbol},
    ship_controller::ShipController,
    tasks::LogisticTaskManager,
};
use log::*;

use super::probe::goto_waypoint_anywhere;

// Entry point for a SHIP_EXPLORER assigned the Survey behaviour: scan the configured
// target system once, then trade it via the logistics planner.
pub async fn run_scanner(ship: ShipController, ac: AgentController) {
    info!("Starting survey scanner for {}", ship.symbol());
    ship.wait_for_transit().await;

    let Some(system) = CONFIG.survey_system.clone() else {
        loop {
            ship.set_state_description("No survey target; idle");
            tokio::time::sleep(tokio::time::Duration::from_secs(600)).await;
        }
    };

    // 1. Survey the target system once (route in + sensor scan; no-op if already done).
    scan_system(&ship, &system).await;

    // 2. Make sure we're actually in the target system (scan_system skips routing once the
    //    survey is done), then trade it like the command ship trades at home. The shared
    //    task manager only plans for the starting system, so use one scoped to this system
    //    (its state is persisted per-system in the DB).
    if ship.system() != system {
        let gate = ship.ctx.universe.get_jumpgate(&system).await;
        ship.set_state_description(&format!("Heading to {} to trade", system));
        goto_waypoint_anywhere(&ship, &gate).await;
    }

    let task_manager =
        LogisticTaskManager::new(&ship.ctx.universe, &ship.ctx.db, &system).await;
    task_manager.set_agent_controller(&ac);
    let task_manager = Arc::new(task_manager);

    // Mirror the explorer trading config: planner on, refresh markets itself, no ship
    // buying / construction, only worthwhile trades.
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
        waypoint_allowlist: None,
        allow_shipbuying: false,
        allow_market_refresh: true,
        allow_construction: false,
        min_profit: 5000,
    };
    info!("[{}] Trading in {}", ship.symbol(), system);
    crate::ship_scripts::logistics::run(ship, task_manager, config, ac).await;
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
