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
use std::collections::HashMap;
use std::sync::OnceLock;
use tokio::sync::Mutex;

// Credit headroom the rush trigger requires: it only latches on once the fleet can
// afford the full escalating estimate to finish AND still keep this much. Since the
// estimate errs high, banking it at trigger time guarantees the finish leaves the
// reserve, so per-buy purchases during the rush run unbuffered (see `tick`).
const RUSH_RESERVE: i64 = 1_000_000;

// DB key (fleet-wide, not per-ship) latching the rush on. Once any hauler determines
// the fleet can afford to finish, all haulers rush together and stay rushing.
const RUSH_LATCH_KEY: &str = "construction_rush_active";

// --- Escalating rush-cost estimate -----------------------------------------------
//
// The game server prices an export as
//   price = base * (1 + (LIMIT*2^(-DILATION*s) - LIMIT - SHIFT) * MARGIN/100)
// with s = netSupply / trade_volume (see ~/spacetraders-dev market pricing). Buying
// drains netSupply, so each successive trade_volume-sized purchase climbs this curve —
// cheap while supply is healthy, exponential once it's drained. We can't read the
// hidden netSupply, so we invert it from the live purchase price, then walk the
// remaining units in trade_volume chunks to sum the escalating cost a rush would pay.
const RUSH_DILATION: f64 = 0.3;
const RUSH_LIMIT: f64 = 4.0;
const RUSH_SHIFT: f64 = 1.0;
// Margin points for goods whose trade volume sits in the 10..100 band (FAB_MATS,
// ADVANCED_CIRCUITRY). The server adds ±1 noise; 8 is the band's base.
const RUSH_MARGIN: f64 = 8.0;

fn rush_base_price(good: &str) -> f64 {
    match good {
        "FAB_MATS" => 1600.0,
        "ADVANCED_CIRCUITRY" => 5200.0,
        _ => panic!("Unknown construction good: {good}"),
    }
}

fn rush_unit_price(base: f64, net_supply: f64, trade_volume: f64) -> f64 {
    let s = net_supply / trade_volume;
    let margin_mult = RUSH_LIMIT * 2f64.powf(-RUSH_DILATION * s) - RUSH_LIMIT - RUSH_SHIFT;
    (base * (1.0 + margin_mult * RUSH_MARGIN / 100.0)).max(10.0)
}

// Invert an observed purchase price back to the hidden netSupply.
fn rush_infer_net_supply(spot: f64, base: f64, trade_volume: f64) -> f64 {
    let rhs = ((spot / base - 1.0) * 100.0 / RUSH_MARGIN + RUSH_LIMIT + RUSH_SHIFT) / RUSH_LIMIT;
    -(rhs.log2()) / RUSH_DILATION * trade_volume
}

// Estimated cost to buy `need` more units now, in trade_volume chunks, with the price
// escalating as each chunk drains supply. Models one continuous burst from a single
// market, so it errs high (real buying spreads across markets and lets them regen) —
// the safe direction for an "can we afford it" gate.
fn rush_cost_for_good(good: &str, spot: i64, trade_volume: i64, need: i64) -> i64 {
    if need <= 0 || trade_volume <= 0 {
        return 0;
    }
    let base = rush_base_price(good);
    let tv = trade_volume as f64;
    let net_supply_real = rush_infer_net_supply(spot as f64, base, tv);
    if !net_supply_real.is_finite() {
        // Price outside the model's domain — fall back to a flat spot estimate.
        return spot.saturating_mul(need);
    }
    // netSupply is an integer server-side; round our inferred value to match.
    let mut net_supply = net_supply_real.round();
    let mut remaining = need;
    let mut total: i64 = 0;
    while remaining > 0 {
        let v = remaining.min(trade_volume);
        let unit = rush_unit_price(base, net_supply, tv).round() as i64;
        total = total.saturating_add(unit.saturating_mul(v));
        net_supply -= v as f64;
        remaining -= v;
    }
    total
}

// Units of `good` currently held across all construction haulers (bought but not yet
// delivered). Each hauler subtracts this so the fleet doesn't over-buy the final stretch:
// `fulfilled` only counts delivered units, so without it a second hauler would re-buy
// what a first is already carrying — costly under rush (premium prices, and excess can
// only be dumped at a loss once the gate is satisfied).
fn fleet_inflight(ac: &AgentController, good: &str) -> i64 {
    ac.ships()
        .iter()
        .filter(|(_, _, job, _)| job.starts_with("jump_gate_hauler"))
        .map(|(_, ship, _, _)| {
            ship.cargo
                .inventory
                .iter()
                .find(|g| g.symbol.as_str() == good)
                .map(|g| g.units)
                .unwrap_or(0)
        })
        .sum()
}

// Fleet-wide reservation of construction-good units a hauler has committed to buy but
// hasn't yet loaded into cargo — keyed by ship so an owner can always reclaim its own.
// `fleet_inflight` only sees units already in a hold, so without this two haulers deciding
// at the same instant would both claim the same outstanding gap and overbuy the tail.
//
// The map is persisted in the DB so the coordination survives a restart (which, per the
// panic-crashes-the-agent gotcha, is not rare): a hauler that crashes mid-navigation keeps
// its reservation, so siblings still avoid its gap, and on restart the owner reclaims its
// own entry (`clear_reservation`, called at the top of every Buying tick) — so a
// reservation can never become a phantom that permanently under-counts the gap.
const RESERVATIONS_KEY: &str = "construction_reservations";

// ship symbol -> (good, units) committed but not yet in cargo.
type Reservations = HashMap<String, (String, i64)>;

// Serializes the reservation read-modify-write across the in-process hauler tasks. The
// reservations live in the DB; this only guards the critical section. Held across the DB
// round-trip (fast) but never across navigation or a buy.
fn reservation_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// Atomically claim up to `max_units` of `good` from the outstanding gap and record this
// ship's commitment in the durable map. Returns the units reserved (0 if siblings already
// cover the gap). Pair with `clear_reservation` once the buy lands or is abandoned.
async fn reserve_units(
    db: &DbClient,
    ac: &AgentController,
    ship_symbol: &str,
    good: &str,
    required: i64,
    fulfilled: i64,
    max_units: i64,
) -> i64 {
    let _guard = reservation_lock().lock().await;
    let mut map: Reservations = db.get_value(RESERVATIONS_KEY).await.unwrap_or_default();
    let inflight = fleet_inflight(ac, good);
    let units = reservation_gap(
        &map,
        ship_symbol,
        good,
        required,
        fulfilled,
        inflight,
        max_units,
    );
    if units > 0 {
        map.insert(ship_symbol.to_string(), (good.to_string(), units));
        db.set_value(RESERVATIONS_KEY, &map).await;
    }
    units
}

// Drop this ship's reservation — after its buy lands in cargo (where `fleet_inflight`
// takes over), when it abandons a buy, and at the top of each Buying tick to reclaim a
// stale entry left by a crash mid-navigation.
async fn clear_reservation(db: &DbClient, ship_symbol: &str) {
    let _guard = reservation_lock().lock().await;
    let mut map: Reservations = db.get_value(RESERVATIONS_KEY).await.unwrap_or_default();
    if map.remove(ship_symbol).is_some() {
        db.set_value(RESERVATIONS_KEY, &map).await;
    }
}

// Pure gap math (testable without DB/AgentController): units still needed after delivered +
// in-cargo + *other* ships' reservations, capped by what fits. This ship's own entry is
// excluded so a stale self-reservation (from a crashed prior tick) doesn't count against
// its fresh decision.
fn reservation_gap(
    map: &Reservations,
    ship_symbol: &str,
    good: &str,
    required: i64,
    fulfilled: i64,
    inflight: i64,
    max_units: i64,
) -> i64 {
    let reserved: i64 = map
        .iter()
        .filter(|(s, (g, _))| s.as_str() != ship_symbol && g == good)
        .map(|(_, (_, u))| *u)
        .sum();
    max_units
        .min(required - fulfilled - inflight - reserved)
        .max(0)
}

// Total escalating cost to finish every remaining material, pricing each good from the
// cheapest export market we currently have data for. Returns None if any material's
// market data is missing, so a missing snapshot can't trigger an over-optimistic rush.
fn estimate_rush_cost(
    ship: &ShipController,
    ac: &AgentController,
    construction: &Construction,
    fab_mat_markets: &[WaypointSymbol],
    adv_circuit_markets: &[WaypointSymbol],
) -> Option<i64> {
    let mut total: i64 = 0;
    for mat in &construction.materials {
        // Don't count units already fulfilled or riding in a hauler's hold.
        let need = mat.required - mat.fulfilled - fleet_inflight(ac, &mat.trade_symbol);
        if need <= 0 {
            continue;
        }
        let markets = match mat.trade_symbol.as_str() {
            "FAB_MATS" => fab_mat_markets,
            "ADVANCED_CIRCUITRY" => adv_circuit_markets,
            _ => panic!("Unknown construction good: {}", mat.trade_symbol),
        };
        // cheapest currently-known market for this good: (purchase_price, trade_volume)
        let mut cheapest: Option<(i64, i64)> = None;
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
            if cheapest.is_none_or(|(p, _)| good.purchase_price < p) {
                cheapest = Some((good.purchase_price, good.trade_volume));
            }
        }
        let (spot, trade_volume) = cheapest?;
        total = total.saturating_add(rush_cost_for_good(
            &mat.trade_symbol,
            spot,
            trade_volume,
            need,
        ));
    }
    Some(total)
}

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
    waypoints.sort_by_key(|w| w.symbol.to_string());
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

#[cfg(test)]
mod rush_cost_tests {
    use super::*;

    // Reproduces the validated offline simulation (matched live prices to ~0.2%):
    // rushing from a drained ADVANCED_CIRCUITRY export and a healthy FAB_MATS export.
    #[test]
    fn matches_reference_simulation() {
        let ac = rush_cost_for_good("ADVANCED_CIRCUITRY", 5959, 20, 206);
        let fab = rush_cost_for_good("FAB_MATS", 1327, 48, 725);
        assert!((ac - 2_491_172).abs() < 5_000, "AC rush cost {ac}");
        assert!((fab - 2_384_663).abs() < 5_000, "FAB rush cost {fab}");
    }

    // One trade-volume batch costs ~spot (flat zone); a deep rush from a drained market
    // is many multiples of spot (the exponential tail we care about).
    #[test]
    fn escalates_with_depth() {
        let one_batch = rush_cost_for_good("ADVANCED_CIRCUITRY", 5959, 20, 20);
        assert!((one_batch - 5959 * 20).abs() < 5959, "one batch ~ spot");

        let deep = rush_cost_for_good("ADVANCED_CIRCUITRY", 5959, 20, 400);
        assert!(
            deep > 5 * 5959 * 400,
            "400u from LIMITED should be >5x spot"
        );
    }

    #[test]
    fn nothing_left_is_free() {
        assert_eq!(rush_cost_for_good("FAB_MATS", 1327, 48, 0), 0);
    }

    // Two haulers reserving back-to-back must not collectively claim more than the gap.
    #[test]
    fn reservations_prevent_overbuy() {
        let mut map = Reservations::new();
        // required 100, fulfilled 40, 0 in cargo -> gap 60. Each hauler wants a 60-unit load.
        let a = reservation_gap(&map, "A", "FAB_MATS", 100, 40, 0, 60);
        assert_eq!(a, 60, "first hauler takes the whole gap");
        map.insert("A".into(), ("FAB_MATS".into(), a)); // A commits

        let b = reservation_gap(&map, "B", "FAB_MATS", 100, 40, 0, 60);
        assert_eq!(b, 0, "second hauler sees A's reservation and takes nothing");

        // A releases (buy abandoned); the gap frees up again, capped by max_units.
        map.remove("A");
        let c = reservation_gap(&map, "B", "FAB_MATS", 100, 40, 0, 20);
        assert_eq!(c, 20, "capped by max_units, not the gap");
    }

    // In-cargo units already count toward the gap without a reservation.
    #[test]
    fn inflight_counts_toward_gap() {
        let map = Reservations::new();
        // required 100, fulfilled 30, 70 in cargo -> gap 0.
        assert_eq!(reservation_gap(&map, "A", "FAB_MATS", 100, 30, 70, 60), 0);
    }

    // A ship's own stale reservation (from a crashed prior tick) must not count against
    // its fresh decision — it reclaims it, so the gap is the same as if it were absent.
    #[test]
    fn own_stale_reservation_excluded() {
        let mut map = Reservations::new();
        map.insert("A".into(), ("FAB_MATS".into(), 30));
        // A re-deciding: its own 30 is ignored -> full gap 60 still available.
        assert_eq!(reservation_gap(&map, "A", "FAB_MATS", 100, 40, 0, 60), 60);
        // But a sibling B does see A's 30 -> gap 30.
        assert_eq!(reservation_gap(&map, "B", "FAB_MATS", 100, 40, 0, 60), 30);
    }

    // A reservation for a different good doesn't affect this good's gap.
    #[test]
    fn other_good_reservation_ignored() {
        let mut map = Reservations::new();
        map.insert("A".into(), ("ADVANCED_CIRCUITRY".into(), 20));
        assert_eq!(reservation_gap(&map, "B", "FAB_MATS", 100, 40, 0, 60), 60);
    }
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

    // This hauler's ordinal, parsed from its "jump_gate_hauler/N" job id. Used to stagger
    // which material each hauler services first, so the fleet fills the materials in
    // parallel instead of every hauler draining the first to 100% before the next.
    let hauler_index: usize = ac
        .ships()
        .into_iter()
        .find(|(sym, _, _, _)| sym == &ship.symbol())
        .and_then(|(_, _, job, _)| job.rsplit('/').next().and_then(|n| n.parse().ok()))
        .unwrap_or(0);

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
            &ac,
            state,
            &jump_gate_symbol,
            &fab_mat_markets,
            &adv_circuit_markets,
            market_offset,
            hauler_index,
        )
        .await;
        if let Some(next_state) = next_state {
            state = next_state;
            db.set_value(&key, &state).await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn tick(
    ship: &ShipController,
    ac: &AgentController,
    state: ConstructionHaulerState,
    jump_gate_symbol: &WaypointSymbol,
    fab_mat_markets: &[WaypointSymbol],
    adv_circuit_markets: &[WaypointSymbol],
    market_offset: usize,
    hauler_index: usize,
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

            // Decide whether to rush. The manual env override forces it; otherwise it
            // auto-enables — and latches fleet-wide — once we can afford to buy out every
            // remaining material at escalating rush prices and still keep RUSH_RESERVE.
            let db = &ship.ctx.db;
            let mut rush_active = CONFIG.override_construction_supply_check
                || db.get_value(RUSH_LATCH_KEY).await.unwrap_or(false);
            if !rush_active
                && let Some(est) =
                    estimate_rush_cost(ship, ac, construction, fab_mat_markets, adv_circuit_markets)
            {
                let available = ship.ctx.ledger.available_credits();
                if available >= est.saturating_add(RUSH_RESERVE) {
                    info!(
                        "Construction rush auto-enabled: available {} >= est rush cost {} + reserve {}",
                        available, est, RUSH_RESERVE
                    );
                    db.set_value(RUSH_LATCH_KEY, &true).await;
                    rush_active = true;
                }
            }

            // Reclaim any reservation this ship left dangling if a prior tick crashed
            // mid-navigation, before making fresh decisions this tick.
            let ship_symbol = ship.symbol();
            clear_reservation(db, &ship_symbol).await;

            // load up on construction goods. Each hauler starts the scan at material
            // `hauler_index` (mod count) so haulers stagger across materials and fill them
            // in parallel, rather than all draining the first material to 100% first.
            let mut incomplete_materials = 0;
            let num_materials = construction.materials.len();
            for offset in 0..num_materials {
                let mat = &construction.materials[(hauler_index + offset) % num_materials];
                // Count units already delivered plus every hauler's in-flight cargo, so
                // the fleet targets the true outstanding gap instead of each hauler
                // re-buying what its siblings already carry.
                let inflight = fleet_inflight(ac, &mat.trade_symbol);
                if mat.fulfilled + inflight >= mat.required {
                    continue;
                }
                incomplete_materials += 1;
                let markets = match mat.trade_symbol.as_str() {
                    "FAB_MATS" => fab_mat_markets,
                    "ADVANCED_CIRCUITRY" => adv_circuit_markets,
                    _ => panic!("Unknown construction good: {}", mat.trade_symbol),
                };
                // Per-buy credit floor. While rushing, buy with no buffer (0): the rush
                // trigger already required available_credits ≥ escalating estimate +
                // RUSH_RESERVE, and the estimate errs high, so finishing is guaranteed to
                // leave the reserve — re-charging it here would only stall the tail. This
                // also drops advanced circuitry's extra buy floor (we're buying everything
                // now, so FAB no longer needs priority). Otherwise keep the extra floor
                // against advanced circuitry, since FAB_MATS are higher priority when
                // credits are low (they're the long pole).
                let credit_buffer = if rush_active {
                    0
                } else {
                    match mat.trade_symbol.as_str() {
                        "FAB_MATS" => 0,
                        "ADVANCED_CIRCUITRY" => 1_000_000,
                        _ => panic!("Unknown construction good: {}", mat.trade_symbol),
                    }
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
                    if should_buy || rush_active {
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

                // Atomically claim our share of the outstanding gap (this ship's hold + a
                // trade_volume chunk), so a sibling deciding at the same instant can't also
                // buy it. The reservation persists until these units land in cargo.
                let max_units = min(good.trade_volume, ship.cargo_space_available());
                let units = reserve_units(
                    db,
                    ac,
                    &ship_symbol,
                    &mat.trade_symbol,
                    mat.required,
                    mat.fulfilled,
                    max_units,
                )
                .await;
                if units <= 0 {
                    // A sibling already covers this material's remaining gap.
                    continue;
                }
                ship.goto_waypoint(market_symbol).await;

                let expected_cost = good.purchase_price * units;
                let credits = ship.ctx.ledger.available_credits();
                if expected_cost > credits - credit_buffer {
                    clear_reservation(db, &ship_symbol).await;
                    debug!(
                        "Insufficient funds to buy {} units of {}. {}/{} (buffer: {})",
                        units, good.symbol, credits, expected_cost, credit_buffer
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
                    return None;
                }
                ship.buy_goods(&good.symbol, units, false).await;
                // Now in cargo; fleet_inflight accounts for it, so drop the reservation.
                clear_reservation(db, &ship_symbol).await;
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
