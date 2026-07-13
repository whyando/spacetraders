use std::{cmp::min, sync::Arc};

use crate::{
    agent_controller::AgentController, logistics_planner::Action, models::LogisticsScriptConfig,
    ship_controller::ShipController, tasks::LogisticTaskManager, universe::WaypointFilter,
};
use log::*;

pub async fn run(
    ship_controller: ShipController,
    taskmanager: Arc<LogisticTaskManager>,
    config: LogisticsScriptConfig,
    ac: AgentController,
) {
    info!("Starting script logistics for {}", ship_controller.symbol());
    ship_controller.wait_for_transit().await;

    let ship_symbol = ship_controller.symbol();
    let system_symbol = ship_controller.system();
    assert_eq!(
        config.use_planner,
        config.planner_config.is_some(),
        "planner_config must be set if use_planner is true"
    );

    // Register the ship with the task manager
    taskmanager
        .register_ship(
            &ship_symbol,
            &system_symbol,
            &config,
            ship_controller.cargo_capacity(),
            ship_controller.engine_speed(),
            ship_controller.fuel_capacity(),
        )
        .await;

    loop {
        // Before taking work: if the ship has no in-progress task, its hold should be
        // empty. Anything in it is stray — e.g. a trade whose sell leg never ran because
        // a crash interrupted it — which silently eats capacity and can overflow the next
        // buy. Clear it here (safe: no in-flight task good to protect).
        if taskmanager.get_next_action(&ship_symbol).is_none() {
            reconcile_stray_cargo(&ship_controller).await;
        }

        // Get next action from task manager
        let action = match taskmanager
            .get_next_task(&ship_symbol, &ship_controller.waypoint())
            .await
        {
            Some(action) => action,
            None => {
                info!(
                    "Ship {} was scheduled no tasks to perform. Sleeping 5-10 minutes.",
                    ship_controller.symbol()
                );
                let rand_seconds = rand::random::<u64>() % 300;
                tokio::time::sleep(tokio::time::Duration::from_secs(300 + rand_seconds)).await;
                continue;
            }
        };

        ship_controller.goto_waypoint(&action.waypoint).await;
        execute_logistics_action(&ship_controller, &action.action, &ac).await;

        // Mark the action as complete
        taskmanager.complete_action(&ship_symbol, &action).await;

        info!(
            "Ship {} completed action at {}",
            ship_controller.symbol(),
            action.waypoint
        );
    }
}

// Dispose of cargo the ship is holding while it owns no task. Called only when the task
// queue is empty, so every held good is stray (a completed task always empties the hold)
// — most commonly a good bought for a trade whose sell leg was lost to a crash. Each stray
// good is sold at an in-system market that buys it (import or exchange), or jettisoned as a
// last resort so it can't permanently occupy the hold. FUEL is never touched — cargo fuel
// is intentional for long jumps.
async fn reconcile_stray_cargo(ship: &ShipController) {
    let stray: Vec<_> = ship
        .cargo_inventory()
        .into_iter()
        .filter(|item| item.symbol != "FUEL")
        .collect();
    if stray.is_empty() {
        return;
    }
    let system = ship.system();
    for item in stray {
        let good = item.symbol;
        warn!(
            "{}: stray cargo {} x{} with no owning task — disposing",
            ship.symbol(),
            good,
            item.units
        );
        // A market buys a good if it imports or exchanges it.
        let mut buyers = ship
            .ctx
            .universe
            .search_waypoints(&system, &[WaypointFilter::Imports(good.clone())])
            .await;
        if buyers.is_empty() {
            buyers = ship
                .ctx
                .universe
                .search_waypoints(&system, &[WaypointFilter::Exchanges(good.clone())])
                .await;
        }
        match buyers.first() {
            Some(dest) => {
                ship.goto_waypoint(&dest.symbol).await;
                ship.refresh_market().await;
                let mut remaining = ship.cargo_good_count(&good);
                while remaining > 0 {
                    let market = ship.ctx.universe.get_market(&ship.waypoint()).unwrap();
                    let Some(trade) = market.data.trade_goods.iter().find(|g| g.symbol == good)
                    else {
                        break;
                    };
                    let units = min(trade.trade_volume, remaining);
                    ship.sell_goods(&good, units, true).await;
                    ship.refresh_market().await;
                    remaining -= units;
                }
                // Market couldn't absorb all of it: jettison the rest to free the hold.
                let leftover = ship.cargo_good_count(&good);
                if leftover > 0 {
                    ship.jettison_cargo(&good, leftover).await;
                }
            }
            None => {
                warn!(
                    "{}: no in-system market buys {}; jettisoning {}",
                    ship.symbol(),
                    good,
                    item.units
                );
                ship.jettison_cargo(&good, item.units).await;
            }
        }
    }
}

async fn execute_logistics_action(ship: &ShipController, action: &Action, ac: &AgentController) {
    match action {
        Action::RefreshMarket => ship.refresh_market().await,
        Action::RefreshShipyard => ship.refresh_shipyard().await,
        Action::BuyGoods(good, units) => {
            let good_count = ship.cargo_good_count(good);
            let mut remaining_to_buy = units - good_count;
            ship.refresh_market().await;
            while remaining_to_buy > 0 {
                // Clamp to free space: the planner sizes `units` against an empty hold,
                // but the ship may carry other cargo (fuel, or stray goods from a
                // crash-interrupted trade). Buying past capacity 400s and — via the
                // panic-on-non-2xx api client — crashes the whole agent. If the hold is
                // full, stop rather than loop forever.
                let space = ship.cargo_space_available();
                if space == 0 {
                    warn!(
                        "{}: hold full ({} units of other cargo), can't buy remaining {} {}",
                        ship.symbol(),
                        ship.cargo_units(),
                        remaining_to_buy,
                        good
                    );
                    break;
                }
                let market = ship.ctx.universe.get_market(&ship.waypoint()).unwrap();
                let trade = market
                    .data
                    .trade_goods
                    .iter()
                    .find(|g| g.symbol == *good)
                    .unwrap();
                let buy_units = min(min(trade.trade_volume, remaining_to_buy), space);
                ship.buy_goods(good, buy_units, true).await;
                ship.refresh_market().await;
                remaining_to_buy -= buy_units;
            }
        }
        Action::SellGoods(good, _units) => {
            let good_count = ship.cargo_good_count(good);
            let mut remaining_to_sell = good_count;
            ship.refresh_market().await;
            while remaining_to_sell > 0 {
                let market = ship.ctx.universe.get_market(&ship.waypoint()).unwrap();
                let trade = market
                    .data
                    .trade_goods
                    .iter()
                    .find(|g| g.symbol == *good)
                    .unwrap();
                let sell_units = min(trade.trade_volume, remaining_to_sell);
                ship.sell_goods(good, sell_units, true).await;
                ship.refresh_market().await;
                remaining_to_sell -= sell_units;
            }
        }
        Action::TryBuyShips => {
            assert!(!ship.is_in_transit());
            info!("Starting buy task for ship {}", ship.ship_symbol);
            ship.dock().await;
            let (bought, _shipyard_waypoints) =
                ac.try_buy_ships(Some(ship.ship_symbol.clone())).await;
            info!("Buy task resulted in {} ships bought", bought.len());
            for ship_symbol in bought {
                ship.debug(&format!("{} Bought ship {}", ship.ship_symbol, ship_symbol));
                ac.spawn_run_ship(ship_symbol).await;
            }
        }
        Action::DeliverConstruction(good, units) => {
            ship.supply_construction(good, *units).await;
        }
        Action::DeliverContract(good, units) => {
            if ship.cargo_good_count(good) == 0 {
                warn!(
                    "Ship {} has no cargo of {}. Assuming action is complete.",
                    ship.ship_symbol, good
                );
                return;
            }

            let contract_id = ac.get_current_contract_id().unwrap();
            ship.deliver_contract(&contract_id, good, *units).await;
            ac.spawn_contract_task();
        }
        _ => {
            panic!("Action not implemented: {:?}", action);
        }
    }
}
