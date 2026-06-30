//!
//! A ship dedicated to hauling resources to the construction of the jump gate.
//!
//! This script does NOT coordinate with the logistic task manager. Which means the logistics task manager
//! needs to be configured not to create construction tasks, or any task involving the construction goods.
//!
use crate::config::CONFIG;
use crate::models::MarketActivity::*;
use crate::models::MarketSupply::*;
use crate::models::MarketType::*;
use crate::{
    agent_controller::AgentController,
    database::DbClient,
    models::{Construction, MarketTradeGood, WaypointSymbol},
    ship_controller::ShipController,
    universe::WaypointFilter,
};
use ConstructionHaulerState::*;
use log::*;
use serde::{Deserialize, Serialize};
use std::cmp::min;

// All export markets for a good in the home system. The gate's demand can spawn more
// than one exporter, so this returns every match (sorted for stable ordering) rather
// than assuming a unique one — the hauler picks among them per trip based on which is
// currently buyable. Must find at least one; zero means the universe data is wrong.
async fn get_export_markets(ship: &ShipController, good: &str) -> Vec<WaypointSymbol> {
    let filters = vec![WaypointFilter::Exports(good.to_string())];
    let system = ship.ctx.starting_system();
    let mut waypoints = ship.ctx.universe.search_waypoints(&system, &filters).await;
    assert!(
        !waypoints.is_empty(),
        "Expected at least 1 export market for {good}, got 0"
    );
    waypoints.sort_by(|a, b| a.symbol.to_string().cmp(&b.symbol.to_string()));
    waypoints.into_iter().map(|w| w.symbol).collect()
}

async fn get_jump_gate(ship: &ShipController) -> WaypointSymbol {
    let system = ship.ctx.starting_system();
    let waypoints = ship
        .ctx
        .universe
        .search_waypoints(&system, &[WaypointFilter::JumpGate])
        .await;
    assert_eq!(waypoints.len(), 1,);
    waypoints[0].symbol.clone()
}

// pub async fn get_probe_shipyard(ship: &ShipController) -> WaypointSymbol {
//     let system = ship.agent_controller.faction_capital();
//     let shipyards = ship.universe.get_system_shipyards_remote(&system).await;
//     let filtered = shipyards
//         .iter()
//         .filter(|sy| sy.ship_types.iter().any(|st| st.ship_type == "SHIP_PROBE"))
//         .collect::<Vec<_>>();
//     assert!(filtered.len() >= 1);
//     filtered[0].symbol.clone()
// }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum ConstructionHaulerState {
    Buying,
    Delivering,
    Completed,
    TerminalState,
}

pub async fn run_hauler(ship: ShipController, db: DbClient, ac: AgentController) {
    info!("Starting script construction_hauler for {}", ship.symbol());
    ship.wait_for_transit().await;

    let jump_gate_symbol = get_jump_gate(&ship).await;
    let fab_mat_markets = get_export_markets(&ship, "FAB_MATS").await;
    let adv_circuit_markets = get_export_markets(&ship, "ADVANCED_CIRCUITRY").await;
    // Stable per-ship offset so that when more than one market is buyable, multiple
    // construction haulers prefer different ones instead of clumping on the cheapest.
    let market_offset: usize = ship.symbol().bytes().map(|b| b as usize).sum();

    let key = format!("construction_state/{}", ship.symbol());
    let mut state: ConstructionHaulerState = db.get_value(&key).await.unwrap_or(Buying);

    loop {
        // Once the gate is built (era advanced past the home build-out) this dedicated
        // hauler is no longer needed — scrap it. Same era cue the mining/siphon fleet
        // retires on, so the ship-config gate (never_purchase) and this scrap can't
        // disagree and rebuy-churn.
        if super::home_phase_done(&ac) {
            return super::scrap::run(ship).await;
        }
        let next_state = tick(
            &ship,
            state,
            &jump_gate_symbol,
            &fab_mat_markets,
            &adv_circuit_markets,
            market_offset,
        )
        .await;
        if let Some(next_state) = next_state {
            state = next_state;
            db.set_value(&key, &state).await;
        }
    }
}

async fn tick(
    ship: &ShipController,
    state: ConstructionHaulerState,
    jump_gate_symbol: &WaypointSymbol,
    fab_mat_markets: &[WaypointSymbol],
    adv_circuit_markets: &[WaypointSymbol],
    market_offset: usize,
) -> Option<ConstructionHaulerState> {
    match state {
        Buying => {
            let construction = ship.ctx.universe.get_construction(jump_gate_symbol).await;
            let construction: &Construction = match &construction.data {
                None => return Some(Completed),
                Some(x) if x.is_complete => return Some(Completed),
                Some(x) => x,
            };
            if ship.cargo_space_available() == 0 {
                return Some(Delivering);
            }

            // load up on construction goods
            let mut incomplete_materials = 0;
            for mat in &construction.materials {
                let holding = ship.cargo_good_count(&mat.trade_symbol);
                if mat.fulfilled + holding >= mat.required {
                    continue;
                }
                incomplete_materials += 1;
                let markets = match mat.trade_symbol.as_str() {
                    "FAB_MATS" => fab_mat_markets,
                    "ADVANCED_CIRCUITRY" => adv_circuit_markets,
                    _ => panic!("Unknown construction good: {}", mat.trade_symbol),
                };
                // Add a credit buffer against advanced circuitry, since FABMATs are higher priority when credits are low
                // because they are the long pole
                let credit_buffer = match mat.trade_symbol.as_str() {
                    "FAB_MATS" => 0,
                    "ADVANCED_CIRCUITRY" => 1_000_000,
                    _ => panic!("Unknown construction good: {}", mat.trade_symbol),
                };
                // Gather the export markets currently worth buying from (supply high
                // enough for their activity level), then pick one: cheapest first,
                // rotated by this ship's offset so multiple haulers split across markets
                // instead of all piling onto the same one.
                let mut buyable: Vec<(WaypointSymbol, MarketTradeGood)> = Vec::new();
                for market_symbol in markets {
                    let Some(market) = ship.ctx.universe.get_market(market_symbol) else {
                        continue;
                    };
                    let Some(good) = market
                        .data
                        .trade_goods
                        .iter()
                        .find(|x| x.symbol == mat.trade_symbol)
                    else {
                        continue;
                    };
                    assert_eq!(good._type, Export);
                    let should_buy = match good.activity.as_ref().unwrap() {
                        Strong => good.supply >= High,
                        _ => good.supply >= Moderate,
                    };
                    if should_buy || CONFIG.override_construction_supply_check {
                        buyable.push((market_symbol.clone(), good.clone()));
                    }
                }
                if buyable.is_empty() {
                    continue;
                }
                buyable.sort_by(|a, b| {
                    a.1.purchase_price
                        .cmp(&b.1.purchase_price)
                        .then_with(|| a.0.to_string().cmp(&b.0.to_string()))
                });
                let (market_symbol, good) = &buyable[market_offset % buyable.len()];

                let required_units = mat.required - holding - mat.fulfilled;
                let units = min(
                    good.trade_volume,
                    min(ship.cargo_space_available(), required_units),
                );
                ship.goto_waypoint(market_symbol).await;

                let expected_cost = good.purchase_price * units;
                let credits = ship.ctx.ledger.available_credits();
                if expected_cost > credits - credit_buffer {
                    debug!(
                        "Insufficient funds to buy {} units of {}. {}/{} (buffer: {})",
                        units, good.symbol, credits, expected_cost, credit_buffer
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    return None;
                }
                ship.buy_goods(&good.symbol, units, false).await;
                ship.refresh_market().await;
                return None;
            }
            // cargo not full and nothing to buy: retry in 60 seconds
            if incomplete_materials == 0 || ship.cargo_units() != 0 {
                return Some(Delivering);
            }

            // Nothing to buy right now: reposition ship to a FAB_MAT export market
            let at_export_market = fab_mat_markets
                .iter()
                .chain(adv_circuit_markets.iter())
                .any(|m| ship.waypoint() == *m);
            if !at_export_market {
                ship.debug("Repositioning to FAB_MAT market");
                let target = &fab_mat_markets[market_offset % fab_mat_markets.len()];
                ship.goto_waypoint(target).await;
                return None;
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
            None
        }
        Delivering => {
            if ship.cargo_empty() {
                return Some(Buying);
            }
            // todo - handle case where materials are no longer needed
            ship.goto_waypoint(jump_gate_symbol).await;
            while let Some(cargo_item) = ship.cargo_first_item() {
                ship.supply_construction(&cargo_item.symbol, cargo_item.units)
                    .await;
            }
            None
        }
        Completed | TerminalState => {
            // Gate is built; nothing left to haul. The actual scrap is left to
            // run_hauler's era check (so we don't scrap-then-rebuy in the brief window
            // before the era advances). Idle until then. TerminalState is a legacy
            // value from older runs, handled the same way.
            ship.set_state_description("Gate built; awaiting scrap");
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            None
        }
    }
}
