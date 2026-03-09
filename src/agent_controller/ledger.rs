/// Track the allocations of current credits of the agent
use log::*;
use std::collections::BTreeMap;
use std::sync::Mutex;

#[derive(Debug)]
#[derive(Default)]
struct ShipEntry {
    reserved_credits: i64,
    // trade_symbol -> (units, total_value)
    goods: BTreeMap<String, (i64, i64)>,
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
        let mut ships = self.ships.lock().unwrap();
        ships.insert(
            ship_symbol.to_string(),
            ShipEntry {
                reserved_credits: amount,
                goods: BTreeMap::new(),
            },
        );
    }

    pub fn register_goods_change(
        &self,
        ship_symbol: &str,
        good: &str,
        units: i64,
        price_per_unit: i64,
    ) {
        let mut ships = self.ships.lock().unwrap();
        let ship_entry = ships.entry(ship_symbol.to_string()).or_default();
        let good_entry = ship_entry.goods.entry(good.to_string()).or_insert((0, 0));
        good_entry.0 += units;
        good_entry.1 += units * price_per_unit;
        // we don't handle it very well if the ship has goods that aren't registered,
        // At sell point, this would result in the ship having negative cargo value, so just bottom out at 0
        if good_entry.0 <= 0 || good_entry.1 <= 0 {
            ship_entry.goods.remove(good);
        }
    }

    pub fn available_credits(&self) -> i64 {
        self.credits() - self.effective_reserved_credits()
    }

    // If a ship has 200k reserved and 150k in goods, it has 50k effective reserved credits
    pub fn effective_reserved_credits(&self) -> i64 {
        let ships = self.ships.lock().unwrap();
        ships
            .values()
            .map(|s| {
                // sum up the reserved credits and the value of the goods
                s.reserved_credits - s.goods.values().map(|(_, v)| v).sum::<i64>()
            })
            .sum()
    }
}
