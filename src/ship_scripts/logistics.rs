use std::{cmp::min, sync::Arc};

use crate::{
    agent_controller::AgentController, logistics_planner::Action, models::LogisticsScriptConfig,
    ship_controller::ShipController, tasks::LogisticTaskManager,
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

async fn execute_logistics_action(ship: &ShipController, action: &Action, ac: &AgentController) {
    match action {
        Action::RefreshMarket => ship.refresh_market().await,
        Action::RefreshShipyard => ship.refresh_shipyard().await,
        Action::BuyGoods(good, units) => {
            let good_count = ship.cargo_good_count(good);
            let mut remaining_to_buy = units - good_count;
            ship.refresh_market().await;
            while remaining_to_buy > 0 {
                let market = ship.ctx.universe.get_market(&ship.waypoint()).unwrap();
                let trade = market
                    .data
                    .trade_goods
                    .iter()
                    .find(|g| g.symbol == *good)
                    .unwrap();
                let buy_units = min(trade.trade_volume, remaining_to_buy);
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
