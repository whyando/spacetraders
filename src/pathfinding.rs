use std::{collections::BTreeMap, sync::Arc};

use crate::{
    api_client::api_models::WaypointDetailed,
    models::{ShipFlightMode, System, WaypointSymbol},
};
use std::cmp::max;

#[allow(non_snake_case)]
const CRUISE_NAV_MODIFIER: f64 = 25.0;
const BURN_NAV_MODIFIER: f64 = 12.5;

#[derive(Debug)]
pub struct Pathfinding {
    waypoints: Arc<BTreeMap<WaypointSymbol, WaypointDetailed>>,
    closest_market: BTreeMap<WaypointSymbol, Option<(WaypointSymbol, i64)>>,
}

pub struct Route {
    pub hops: Vec<(WaypointSymbol, Edge, bool, bool)>,
    pub min_travel_duration: i64,
    pub req_terminal_fuel: i64,
}

impl Pathfinding {
    pub fn new(waypoints: Vec<WaypointDetailed>) -> Pathfinding {
        let mut waypoint_map: BTreeMap<WaypointSymbol, WaypointDetailed> = BTreeMap::new();
        let mut closest_market: BTreeMap<WaypointSymbol, Option<(WaypointSymbol, i64)>> =
            BTreeMap::new();
        for waypoint in &waypoints {
            waypoint_map.insert(waypoint.symbol.clone(), waypoint.clone());
            if waypoint.is_market() {
                continue;
            }
            let closest_opt = waypoints
                .iter()
                .filter(|w| w.is_market())
                .map(|w| {
                    let dist = waypoint.distance(w);
                    (w.symbol.clone(), dist)
                })
                .min_by_key(|(_symbol, distance)| *distance);
            closest_market.insert(waypoint.symbol.clone(), closest_opt);
        }
        Pathfinding {
            waypoints: Arc::new(waypoint_map),
            closest_market,
        }
    }

    pub fn get_route(
        &self,
        src_symbol: &WaypointSymbol,
        dest_symbol: &WaypointSymbol,
        speed: i64,
        start_fuel: i64, // ruins the cacheability slightly, since the graph changes
        fuel_capacity: i64,
    ) -> Route {
        use pathfinding::directed::dijkstra::dijkstra;
        // log::debug!(
        //     "Finding route from {} to {} sp: {} sf: {} fc: {}",
        //     src_symbol,
        //     dest_symbol,
        //     speed,
        //     start_fuel,
        //     fuel_capacity
        // );

        let src = self.waypoints.get(src_symbol).unwrap();
        let dst = self.waypoints.get(dest_symbol).unwrap();
        let dest_is_market = dst.is_market();
        let src_is_market = src.is_market();
        let req_escape_fuel = if !dst.is_market() {
            let closest = self
                .closest_market
                .get(dest_symbol)
                .unwrap()
                .as_ref()
                .expect("No market");
            closest.1 // assumes CRUISE
        } else {
            0
        };

        // Route edge conditions:
        // - if src is not a market: the first hop must be <= start_fuel
        // - if dest is not a market, with the closest market X away,
        //   then the last hop must be <= max_fuel - X from a market
        //                          or <= start_fuel - X from a non-market src
        let path: (Vec<WaypointSymbol>, i64) = dijkstra(
            src_symbol,
            |x_symbol| {
                let x = self.waypoints.get(x_symbol).unwrap();
                // start with market <-> market edges
                let mut edges = if x.is_market() {
                    self.waypoints
                        .iter()
                        .filter(|(_y_symbol, y)| y.is_market())
                        .filter_map(|(y_symbol, y)| {
                            if x_symbol == y_symbol {
                                return None;
                            }
                            if let Some(e) = edge(x, y, speed, fuel_capacity) {
                                Some((y_symbol.clone(), e.travel_duration))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                } else {
                    vec![]
                };
                // add non-market -> market edges ( fuel_cost <= start_fuel )
                if !src_is_market && x_symbol == src_symbol {
                    let edges1 = self
                        .waypoints
                        .iter()
                        .filter(|(_y_symbol, y)| y.is_market())
                        .filter_map(|(y_symbol, y)| {
                            if let Some(e) = edge(x, y, speed, start_fuel) {
                                Some((y_symbol.clone(), e.travel_duration))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                    edges.extend(edges1);
                }
                // add market -> non-market edge ( fuel_cost <= max_fuel - req_escape_fuel )
                if !dest_is_market && x_symbol != dest_symbol
                    && let Some(e) = edge(x, dst, speed, fuel_capacity - req_escape_fuel) {
                        edges.push((dest_symbol.clone(), e.travel_duration));
                    }
                // finally add non-market -> non-market edge ( fuel_cost <= start_fuel - req_escape_fuel )
                if !src_is_market && !dest_is_market && x_symbol == src_symbol
                    && let Some(e) = edge(src, dst, speed, start_fuel - req_escape_fuel) {
                        edges.push((dest_symbol.clone(), e.travel_duration));
                    }
                edges
            },
            |x_symbol| *x_symbol == *dest_symbol,
        )
        .expect("No path found");

        let hops = path
            .0
            .iter()
            .zip(path.0.iter().skip(1))
            .map(|(a_symbol, b_symbol)| {
                let a = self.waypoints.get(a_symbol).unwrap();
                let b = self.waypoints.get(b_symbol).unwrap();
                let fuel_max = match (a.is_market(), b.is_market()) {
                    (true, true) => fuel_capacity,
                    (true, false) => fuel_capacity - req_escape_fuel,
                    (false, true) => start_fuel,
                    (false, false) => start_fuel - req_escape_fuel,
                };
                let e = edge(a, b, speed, fuel_max).unwrap();
                (b_symbol.clone(), e, a.is_market(), b.is_market())
            })
            .collect();
        Route {
            hops,
            min_travel_duration: path.1,
            req_terminal_fuel: req_escape_fuel,
        }
    }
}

impl WaypointDetailed {
    pub fn distance(&self, other: &WaypointDetailed) -> i64 {
        if self.symbol == other.symbol {
            return 0;
        }
        let distance2 = (self.x - other.x).pow(2) + (self.y - other.y).pow(2);
        max(1, (distance2 as f64).sqrt().round() as i64)
    }
}

impl System {
    pub fn distance(&self, other: &System) -> i64 {
        if self.symbol == other.symbol {
            return 0;
        }
        let distance2 = (self.x - other.x).pow(2) + (self.y - other.y).pow(2);
        max(1, (distance2 as f64).sqrt().round() as i64)
    }
}

pub struct Edge {
    pub distance: i64,
    pub travel_duration: i64,
    pub fuel_cost: i64,
    pub flight_mode: ShipFlightMode,
}

pub fn edge(a: &WaypointDetailed, b: &WaypointDetailed, speed: i64, fuel_max: i64) -> Option<Edge> {
    let distance = a.distance(b);

    // burn
    if 2 * distance <= fuel_max {
        let travel_duration =
            (15.0 + BURN_NAV_MODIFIER / (speed as f64) * (distance as f64)).round() as i64;
        return Some(Edge {
            distance,
            travel_duration,
            fuel_cost: 2 * distance,
            flight_mode: ShipFlightMode::Burn,
        });
    }

    // cruise
    if distance <= fuel_max {
        let travel_duration =
            (15.0 + CRUISE_NAV_MODIFIER / (speed as f64) * (distance as f64)).round() as i64;
        return Some(Edge {
            distance,
            travel_duration,
            fuel_cost: distance,
            flight_mode: ShipFlightMode::Cruise,
        });
    }
    None
}
