/// Track the allocations of current credits of the agent, plus the
/// weighted-average cost basis of in-transit cargo so realized trade profit can
/// be computed at sell time (proceeds - cost basis of the units sold).
use log::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct GoodLot {
    units: i64,
    // total purchase cost of the currently-held units (weighted-average basis)
    cost_basis: i64,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct ShipEntry {
    reserved_credits: i64,
    goods: BTreeMap<String, GoodLot>,
}

impl ShipEntry {
    fn goods_value(&self) -> i64 {
        self.goods.values().map(|l| l.cost_basis).sum()
    }
}

#[derive(Debug)]
pub struct Ledger {
    total_credits: Mutex<i64>,
    ships: Mutex<BTreeMap<String, ShipEntry>>,
}

impl Ledger {
    pub fn new(start_credits: i64) -> Self {
        Ledger {
            total_credits: Mutex::new(start_credits),
            ships: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn set_credits(&self, credits: i64) {
        *self.total_credits.lock().unwrap() = credits;
    }

    pub fn credits(&self) -> i64 {
        *self.total_credits.lock().unwrap()
    }

    pub fn reserve_credits(&self, ship_symbol: &str, amount: i64) {
        debug!("Setting {} credits reserved for {}", amount, ship_symbol);
        // Preserve any in-transit cargo basis; only (re)set the reservation.
        let mut ships = self.ships.lock().unwrap();
        ships.entry(ship_symbol.to_string()).or_default().reserved_credits = amount;
    }

    // Record a purchase of `units` into the ship's cargo at `price_per_unit`,
    // adding to the held cost basis.
    pub fn register_purchase(&self, ship_symbol: &str, good: &str, units: i64, price_per_unit: i64) {
        if units <= 0 {
            return;
        }
        let mut ships = self.ships.lock().unwrap();
        let lot = ships
            .entry(ship_symbol.to_string())
            .or_default()
            .goods
            .entry(good.to_string())
            .or_default();
        lot.units += units;
        lot.cost_basis += units * price_per_unit;
    }

    // Record a sale of `units` at `price_per_unit`, removing the proportional
    // cost basis. Returns realized profit = proceeds - cost basis of the units
    // sold. Goods with no tracked basis (e.g. mined/siphoned, or bought outside
    // the trade flow) are treated as zero-cost, so the full proceeds are profit.
    pub fn register_sale(&self, ship_symbol: &str, good: &str, units: i64, price_per_unit: i64) -> i64 {
        if units <= 0 {
            return 0;
        }
        let proceeds = units * price_per_unit;
        let cost = self.remove_basis(ship_symbol, good, units);
        proceeds - cost
    }

    // Remove `units` of cost basis for a non-sale outflow (donation, jettison,
    // refuel-from-cargo). Returns the cost basis removed.
    pub fn register_consumption(&self, ship_symbol: &str, good: &str, units: i64) -> i64 {
        if units <= 0 {
            return 0;
        }
        self.remove_basis(ship_symbol, good, units)
    }

    // Shared helper: drop up to `units` from the lot, returning the basis removed.
    fn remove_basis(&self, ship_symbol: &str, good: &str, units: i64) -> i64 {
        let mut ships = self.ships.lock().unwrap();
        let Some(ship) = ships.get_mut(ship_symbol) else {
            return 0;
        };
        let Some(lot) = ship.goods.get_mut(good) else {
            return 0;
        };
        if units >= lot.units {
            // selling/removing at least everything we have basis for
            let c = lot.cost_basis;
            ship.goods.remove(good);
            c
        } else {
            let c = lot.cost_basis * units / lot.units;
            lot.units -= units;
            lot.cost_basis -= c;
            c
        }
    }

    pub fn available_credits(&self) -> i64 {
        self.credits() - self.effective_reserved_credits()
    }

    // If a ship has 200k reserved and 150k of cargo basis, it has 50k effective
    // reserved credits. Clamped at 0 per ship so ships with cargo but no
    // reservation (miners) don't push the fleet total negative.
    pub fn effective_reserved_credits(&self) -> i64 {
        let ships = self.ships.lock().unwrap();
        ships
            .values()
            .map(|s| (s.reserved_credits - s.goods_value()).max(0))
            .sum()
    }

    // Total cost basis of cargo currently held across the fleet (in-transit).
    pub fn cargo_value(&self) -> i64 {
        let ships = self.ships.lock().unwrap();
        ships.values().map(|s| s.goods_value()).sum()
    }

    // Serialize the per-ship reservations + cargo cost basis for persistence, so
    // a restart doesn't lose the basis of in-transit cargo (which would make the
    // next sale read as 100% profit). Reservations are also rebuilt by the
    // controller as it re-assigns jobs, but persisting them is harmless.
    pub fn snapshot(&self) -> LedgerSnapshot {
        LedgerSnapshot {
            ships: self.ships.lock().unwrap().clone(),
        }
    }

    pub fn restore(&self, snapshot: LedgerSnapshot) {
        *self.ships.lock().unwrap() = snapshot.ships;
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LedgerSnapshot {
    ships: BTreeMap<String, ShipEntry>,
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn realized_profit_uses_weighted_cost_basis() {
        let l = Ledger::new(1_000_000);
        // buy 100 @ 50 then 100 @ 70  => 200 units, basis 12_000 (avg 60)
        l.register_purchase("S", "FOOD", 100, 50);
        l.register_purchase("S", "FOOD", 100, 70);
        assert_eq!(l.cargo_value(), 12_000);
        // sell 100 @ 90: cost basis removed = 12000 * 100/200 = 6000; profit 3000
        assert_eq!(l.register_sale("S", "FOOD", 100, 90), 9_000 - 6_000);
        assert_eq!(l.cargo_value(), 6_000);
        // sell remaining 100 @ 40 (a loss): basis 6000, proceeds 4000 => -2000
        assert_eq!(l.register_sale("S", "FOOD", 100, 40), -2_000);
        assert_eq!(l.cargo_value(), 0);
    }

    #[test]
    fn untracked_goods_are_pure_profit() {
        // mined/siphoned goods were never registered as a purchase
        let l = Ledger::new(0);
        assert_eq!(l.register_sale("MINER", "IRON_ORE", 50, 30), 1_500);
    }

    #[test]
    fn effective_reserved_clamps_per_ship() {
        let l = Ledger::new(1_000_000);
        // a hauler reserves 200k and holds 150k of cargo basis => 50k effective
        l.reserve_credits("H", 200_000);
        l.register_purchase("H", "FUEL", 1_500, 100); // 150k basis
        assert_eq!(l.effective_reserved_credits(), 50_000);
        // a miner with cargo but no reservation contributes 0, not negative
        l.register_purchase("M", "ORE", 100, 100);
        assert_eq!(l.effective_reserved_credits(), 50_000);
        // reservations survive being re-set (cargo basis preserved)
        l.reserve_credits("H", 200_000);
        assert_eq!(l.effective_reserved_credits(), 50_000);
    }
}
