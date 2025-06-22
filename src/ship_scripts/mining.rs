use std::cmp::min;

use crate::api_client::api_models::WaypointDetailed;
use crate::models::MarketType::*;
use crate::ship_controller::ShipController;
use crate::universe::WaypointFilter;
use crate::{database::DbClient, models::*};
use MiningShuttleState::*;
use lazy_static::lazy_static;
use log::*;
use serde::{Deserialize, Serialize};

async fn sell_location(ship: &ShipController, cargo_symbol: &str) -> Option<WaypointSymbol> {
    let mut markets = Vec::new();
    let waypoints: Vec<WaypointDetailed> = ship.universe.get_system_waypoints(&ship.system()).await;
    for waypoint in &waypoints {
        if waypoint.is_market() {
            let market_remote = ship.universe.get_market_remote(&waypoint.symbol).await;
            let market_opt = ship.universe.get_market(&waypoint.symbol);
            markets.push((market_remote, market_opt));
        }
    }
    let sell_trade_good = markets
        .iter()
        .filter_map(|(_, market_opt)| match market_opt {
            Some(market) => {
                let market_symbol = market.data.symbol.clone();
                let trade = market
                    .data
                    .trade_goods
                    .iter()
                    .find(|g| g.symbol == cargo_symbol);
                trade.map(|trade| (market_symbol, trade))
            }
            None => None,
        })
        // sell filters
        .filter(|(_, trade)| match trade._type {
            // !! selling IRON_ORE, COPPER_ORE, SILICON_CRYSTALS, and QUARTZ_SAND at import only gives the result we want for construction
            Export => false,
            Exchange => false,
            Import => true,
        })
        // price is a good enough approximation of supply
        .max_by_key(|(_, trade)| trade.sell_price);
    sell_trade_good.map(|(market_symbol, _)| market_symbol)
}

async fn engineered_asteroid_location(ship: &ShipController) -> WaypointSymbol {
    let waypoints = ship
        .universe
        .search_waypoints(&ship.system(), &vec![WaypointFilter::EngineeredAsteroid])
        .await;
    assert!(waypoints.len() == 1);
    waypoints[0].symbol.clone()
}

pub async fn run_surveyor(ship: ShipController) {
    info!("Starting script surveyor for {}", ship.symbol());
    ship.wait_for_transit().await;

    let asteroid_location = engineered_asteroid_location(&ship).await;
    ship.goto_waypoint(&asteroid_location).await;

    loop {
        // Automatically pushes to the survey manager
        ship.survey().await;
    }
}

pub async fn run_mining_drone(ship: ShipController) {
    info!("Starting script extraction_drone for {}", ship.symbol());
    ship.wait_for_transit().await;

    let asteroid_location = engineered_asteroid_location(&ship).await;
    ship.goto_waypoint(&asteroid_location).await;

    loop {
        let should_extract = ship.cargo_space_available() >= 4;
        if should_extract {
            // wait for cooldown before taking survey, helps to get a non-exhausted one
            ship.wait_for_cooldown().await;
            // get survey + extract
            let survey = ship
                .agent_controller
                .survey_manager
                .get_survey(&asteroid_location)
                .await;
            let survey = match survey {
                Some(s) => s,
                None => {
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    continue;
                }
            };
            ship.extract_survey(&survey).await;

            // jettison
            for (cargo, units) in ship.cargo_map() {
                if JETTISON_GOODS.contains(&cargo.as_str()) {
                    ship.jettison_cargo(&cargo, units).await;
                }
            }
        } else {
            // transfer goods to shuttle, and wait till completed
            debug!("Mining drone transfer initiated");
            ship.transfer_cargo().await;
            debug!("Mining drone transfer completed");
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum MiningShuttleState {
    Loading,
    Selling,
}

lazy_static! {
    static ref SELL_GOODS: Vec<&'static str> =
        vec!["SILICON_CRYSTALS", "COPPER_ORE", "IRON_ORE", "QUARTZ_SAND",];
    static ref JETTISON_GOODS: Vec<&'static str> = vec!["ICE_WATER", "ALUMINUM_ORE",];
}

pub async fn run_shuttle(ship: ShipController, db: DbClient) {
    info!("Starting script extraction shuttle for {}", ship.symbol());
    ship.wait_for_transit().await;

    let asteroid_location = engineered_asteroid_location(&ship).await;

    let key = format!("extract_shuttle_state/{}", ship.symbol());
    let mut state: MiningShuttleState = db.get_value(&key).await.unwrap_or(Loading);

    loop {
        match state {
            Loading => {
                if ship.cargo_space_available() == 0 {
                    state = Selling;
                    db.set_value(&key, &state).await;
                    continue;
                }
                ship.goto_waypoint(&asteroid_location).await;
                ship.orbit().await;
                ship.receive_cargo().await;
            }
            Selling => {
                if ship.cargo_empty() {
                    state = Loading;
                    db.set_value(&key, &state).await;
                    continue;
                }
                // !! a smarter selling order would be good here:
                // we risk navigating away from a market even though eg copper_ore and iron_ore are both in the same market
                while let Some(cargo) = ship.cargo_first_item() {
                    if SELL_GOODS.contains(&cargo.symbol.as_str()) {
                        let sell_location = sell_location(&ship, &cargo.symbol).await;
                        match sell_location {
                            Some(sell_location) => {
                                ship.goto_waypoint(&sell_location).await;
                                ship.refresh_market().await;
                                while ship.cargo_good_count(&cargo.symbol) != 0 {
                                    let holding = ship.cargo_good_count(&cargo.symbol);
                                    let market = ship.universe.get_market(&sell_location).unwrap();
                                    let market_good = market
                                        .data
                                        .trade_goods
                                        .iter()
                                        .find(|g| g.symbol == cargo.symbol)
                                        .unwrap();
                                    let units = min(market_good.trade_volume, holding);
                                    assert!(units > 0);
                                    ship.sell_goods(&cargo.symbol, units, false).await;
                                    let new_units = ship.cargo_good_count(&cargo.symbol);
                                    assert!(new_units == holding - units);
                                    ship.refresh_market().await;
                                }
                            }
                            None => {
                                warn!(
                                    "No sell location found for {}. Retry in 60 seconds.",
                                    cargo.symbol
                                );
                                tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                                continue;
                            }
                        }
                    } else if JETTISON_GOODS.contains(&cargo.symbol.as_str()) {
                        ship.jettison_cargo(&cargo.symbol, cargo.units).await;
                    } else {
                        panic!("Unexpected cargo: {}", cargo.symbol);
                    }
                }
            }
        }
    }
}
