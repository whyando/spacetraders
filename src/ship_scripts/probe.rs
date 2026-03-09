use crate::{models::ProbeScriptConfig, ship_controller::ShipController};
use chrono::{DateTime, Duration, Utc};
use lazy_static::lazy_static;
use log::*;
use std::ops::Add as _;

lazy_static! {
    static ref MARKET_REFRESH_INTERVAL: Duration = Duration::try_minutes(6).unwrap();
    static ref SHIPYARD_REFRESH_INTERVAL: Duration = Duration::try_minutes(60).unwrap();
}

pub async fn run(ship_controller: ShipController, config: &ProbeScriptConfig) {
    if config.waypoints.len() == 1 {
        probe_single_location(ship_controller, config).await;
    } else {
        probe_multiple_locations(ship_controller, config).await;
    }
}

// Roaming refresh logic is less rate limit efficient
// - doesn't take into account whether the market has been refreshed recently
// - uses extra api requests to move between waypoints
// Additionally, cannot be used to buy ships
pub async fn probe_multiple_locations(ship: ShipController, config: &ProbeScriptConfig) {
    assert!(config.refresh_market);

    let waypoint_symbols = config
        .waypoints
        .iter()
        .map(|x| x.to_string())
        .collect::<Vec<String>>()
        .join(", ");
    info!(
        "Starting roaming probe script for {} - {}",
        ship.symbol(),
        waypoint_symbols
    );
    ship.wait_for_transit().await;

    let mut waypoints = vec![];
    for waypoint_symbol in &config.waypoints {
        let waypoint = ship.universe.detailed_waypoint(waypoint_symbol).await;
        waypoints.push(waypoint);
    }

    // Random sleep for a gentler startup
    let rand_start_sleep = rand::random::<u64>() % 60;
    tokio::time::sleep(tokio::time::Duration::from_secs(rand_start_sleep)).await;
    let mut last_cycle_start: Option<DateTime<Utc>> = None;
    loop {
        if let Some(last_cycle_start) = last_cycle_start {
            let sleep_duration =
                last_cycle_start + Duration::try_minutes(15).unwrap() - chrono::Utc::now();
            if sleep_duration > Duration::zero() {
                debug!("Sleeping for {:.3}s", sleep_duration.num_seconds() as f64);
                tokio::time::sleep(sleep_duration.to_std().unwrap()).await;
            }
        }
        last_cycle_start = Some(chrono::Utc::now());
        for waypoint in &waypoints {
            ship.goto_waypoint(&waypoint.symbol).await;
            ship.refresh_market().await;

            if waypoint.is_shipyard() {
                ship.refresh_shipyard().await;

                // // Try to buy ships (DISABLED)
                // info!("Starting routine buy task for probe {}", ship.ship_symbol);
                // ship.dock().await; // don't need to dock, but do so anyway to clear 'InTransit' status
                // let (bought, _shipyard_waypoints) = ship
                //     .agent_controller
                //     .try_buy_ships(Some(ship.ship_symbol.clone()))
                //     .await;
                // info!("Routine buy task resulted in {} ships bought", bought.len());
                // for ship_symbol in bought {
                //     debug!("{} Bought ship {}", ship.ship_symbol, ship_symbol);
                //     ship.agent_controller.spawn_run_ship(ship_symbol).await;
                // }
            }
        }
    }
}

// Sit at a single location, refreshing market and shipyards (when needed)
// capable of being used to buy ships
pub async fn probe_single_location(ship_controller: ShipController, config: &ProbeScriptConfig) {
    assert_eq!(config.waypoints.len(), 1);
    let waypoint_symbol = &config.waypoints[0];
    info!(
        "Starting script probe for {} - {}",
        ship_controller.symbol(),
        waypoint_symbol
    );
    ship_controller.wait_for_transit().await;
    let waypoint = ship_controller
        .universe
        .detailed_waypoint(waypoint_symbol)
        .await;

    if ship_controller.system() != waypoint.system_symbol {
        // Assume we can do a single jump to the correct system
        // nav to jumpgate
        let jumpgate_src = ship_controller
            .universe
            .get_jumpgate(&ship_controller.system())
            .await;
        let jumpgate_dest = ship_controller
            .universe
            .get_jumpgate(&waypoint.system_symbol)
            .await;
        ship_controller.goto_waypoint(&jumpgate_src).await;
        // jump to correct system
        ship_controller.jump(&jumpgate_dest).await;
    }

    ship_controller.goto_waypoint(waypoint_symbol).await;
    ship_controller.dock().await; // don't need to dock, but do so anyway to clear 'InTransit' status

    if !config.refresh_market {
        return;
    }

    // Random sleep for a gentler startup
    let rand_start_sleep = rand::random::<u64>() % 60;
    tokio::time::sleep(tokio::time::Duration::from_secs(rand_start_sleep)).await;

    loop {
        let now = chrono::Utc::now();
        let mut next: DateTime<Utc> = now + Duration::try_minutes(15).unwrap();
        if waypoint.is_market() {
            let market = ship_controller.universe.get_market(waypoint_symbol);
            let next_refresh = match market {
                Some(market) => market.timestamp.add(*MARKET_REFRESH_INTERVAL),
                None => now,
            };
            if next_refresh <= now {
                debug!("Refreshing market {}", waypoint_symbol);
                ship_controller.refresh_market().await;
            }
            next = std::cmp::min(next, next_refresh);
        }

        if waypoint.is_shipyard() {
            let shipyard = ship_controller.universe.get_shipyard(waypoint_symbol);
            let next_refresh = match shipyard {
                Some(market) => market.timestamp + *SHIPYARD_REFRESH_INTERVAL,
                None => now,
            };
            if next_refresh <= now {
                debug!("Refreshing shipyard {}", waypoint_symbol);
                ship_controller.refresh_shipyard().await;
            }
            next = std::cmp::min(next, next_refresh);
        }

        let sleep_duration = next - now;
        if sleep_duration > Duration::zero() {
            debug!("Sleeping for {:.3}s", sleep_duration.num_seconds() as f64);
            tokio::time::sleep(sleep_duration.to_std().unwrap()).await;
        }
    }

    // info!("Finished script probe for {}", ship_controller.symbol());
}
