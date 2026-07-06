use std::sync::Arc;

use chrono::Duration;

use crate::{
    agent_controller::AgentController,
    database::DbClient,
    models::{LogisticsScriptConfig, PlanLength, PlannerConfig},
    ship_controller::ShipController,
    tasks::LogisticTaskManager,
};
use log::*;

use super::probe::goto_waypoint_anywhere;

// Trade a single high-value (P(T5) >= 0.5) system. The trader reserves the nearest
// such system reachable over the jumpgate network, jumps there (no warping), and
// runs the logistics planner against that system's markets.
pub async fn run_t5_trader(ship: ShipController, _db: DbClient, ac: AgentController) {
    info!("Starting script t5_trader for {}", ship.symbol());
    ship.wait_for_transit().await;

    let Some(system) = ac.get_t5_system_reservation(&ship.symbol()).await else {
        ship.set_state_description("No target");
        return;
    };

    // Travel to the reserved system over the jumpgate network only — no warping.
    // The reservation is jumpgate-reachable from our home gate and the trader was
    // bought in the (jumpgate-reachable) capital, so a pure-jump route always exists.
    if ship.system() != system {
        ship.set_state_description(&format!("Navigating to {}", system));
        let gate = ship.ctx.universe.get_jumpgate(&system).await;
        goto_waypoint_anywhere(&ship, &gate).await;
    }

    // Our cached view of this system can predate other agents charting its markets:
    // gate systems are snapshotted once at startup, when their market waypoints may
    // still have been uncharted (so stored as non-markets) and never refreshed since.
    // Re-pull traits so we discover the now-charted markets to trade.
    ship.ctx.universe.refresh_system_waypoints(&system).await;

    // A newly-reserved T5 system is often still entirely UNCHARTED for us — nobody has
    // charted its markets, so refresh_system_waypoints above learns nothing and the
    // planner would see zero markets and schedule no tasks. Discover the markets (and
    // shipyards) via a trait-filtered query, which matches even uncharted waypoints, so
    // the planner has a market set to plan over. The RefreshMarket tasks that follow
    // then visit each market, chart it, and record live prices. Re-read waypoints after
    // so the on-market check and planner see the freshly-discovered markets.
    let (n_markets, n_shipyards) = ship.ctx.universe.discover_system_markets(&system).await;
    info!(
        "T5 trader {} discovered {} markets, {} shipyards in {}",
        ship.symbol(),
        n_markets,
        n_shipyards,
        system
    );
    let waypoints = ship.ctx.universe.get_system_waypoints(&system).await;

    // The planner indexes a ship's start waypoint into the system's market set (and
    // panics on a miss). Make sure we end up on a market before trading; from then on
    // trading keeps us market-to-market.
    let on_market = waypoints
        .iter()
        .any(|w| w.symbol == ship.waypoint() && w.is_market());
    if !on_market {
        let here = waypoints.iter().find(|w| w.symbol == ship.waypoint());
        let nearest_market =
            waypoints
                .iter()
                .filter(|w| w.is_market())
                .min_by_key(|w| match here {
                    Some(h) => (w.x - h.x).pow(2) + (w.y - h.y).pow(2),
                    None => 0,
                });
        if let Some(m) = nearest_market {
            ship.set_state_description(&format!("Repositioning to market {} to trade", m.symbol));
            goto_waypoint_anywhere(&ship, &m.symbol).await;
        }
    }

    // Use a task manager scoped to this system — the shared one (ac.task_manager)
    // only plans for the starting system, so its market set wouldn't contain this
    // system's waypoints. State is persisted per-system in the DB.
    let task_manager = LogisticTaskManager::new(&ship.ctx.universe, &ship.ctx.db, &system).await;
    task_manager.set_agent_controller(&ac);
    let task_manager = Arc::new(task_manager);

    info!("T5 trader trading in target system {}", system);
    ship.set_state_description(&format!("Trading in {}", system));
    let config = LogisticsScriptConfig {
        use_planner: true,
        planner_config: Some(PlannerConfig {
            plan_length: PlanLength::Ramping(Duration::seconds(30), Duration::minutes(10), 1.85),
            max_compute_time: Duration::seconds(5),
        }),
        waypoint_allowlist: None,
        allow_shipbuying: false,
        allow_market_refresh: true,
        allow_construction: false,
        min_profit: 5000,
    };
    crate::ship_scripts::logistics::run(ship, task_manager, config, ac).await;
}
