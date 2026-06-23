use crate::{
    agent_controller::AgentController,
    database::DbClient,
    models::{LogisticsScriptConfig, PlanLength, PlannerConfig},
    ship_controller::ShipController,
};
use chrono::Duration;
use log::*;

// Trade a single high-value (P(T5) >= 0.5) system. The trader reserves the nearest
// such system reachable over the jumpgate network, jumps there (no warping), and
// runs the logistics planner against that system's markets.
pub async fn run_t5_trader(ship: ShipController, _db: DbClient, ac: AgentController) {
    info!("Starting script t5_trader for {}", ship.symbol());
    ship.wait_for_transit().await;

    let Some(target) = ac.get_t5_system_reservation(&ship.symbol()).await else {
        ship.set_state_description("No target");
        return;
    };

    // Travel to the reserved system over the jumpgate network only — no warping.
    // The reservation is jumpgate-reachable from our home gate and the trader was
    // bought in the (jumpgate-reachable) capital, so a pure-jump route always exists;
    // goto_waypoint_anywhere routes purely over charted gates.
    if ship.system() != target {
        ship.set_state_description(&format!("Navigating to {}", target));
        let target_gate = ship.ctx.universe.get_jumpgate(&target).await;
        crate::ship_scripts::probe::goto_waypoint_anywhere(&ship, &target_gate).await;
    }
    assert_eq!(ship.system(), target);

    info!("T5 trader trading in target system {}", target);
    ship.set_state_description(&format!("Trading in {}", target));

    let task_manager = ac.task_manager.clone();
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
    crate::ship_scripts::logistics::run(ship.clone(), task_manager, config, ac).await;
}
