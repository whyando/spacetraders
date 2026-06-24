use crate::models::{SystemSymbol, WaypointSymbol};

#[derive(Debug, Clone)]
pub struct Waypoint {
    pub id: i64,
    pub symbol: WaypointSymbol,
    pub waypoint_type: String,
    pub x: i64,
    pub y: i64,
    pub details: Option<WaypointDetails>,
}

#[derive(Debug, Clone)]
pub struct WaypointDetails {
    pub is_market: bool,
    pub is_shipyard: bool,
    pub is_uncharted: bool,
    pub is_under_construction: bool,
}

#[derive(Debug, Clone)]
pub struct System {
    pub symbol: SystemSymbol,
    pub system_type: String,
    pub x: i64,
    pub y: i64,
    pub waypoints: Vec<Waypoint>,
}

// P(T5) priors. Closed-form posterior over 5 generation tiers: per planet
// independently gets a station with prob q_t (Binomial(p, q_t)); planet count
// p ~ round(N(t+1, sigma)). The binomial coefficient C(p,o) cancels across tiers.
// See docs/src/t5-trading.md for the derivation and the wider t5-trading subsystem.
const T5_W: [f64; 5] = [100.0, 100.0, 50.0, 20.0, 20.0]; // generation prior w_t
const T5_MU: [f64; 5] = [2.0, 3.0, 4.0, 5.0, 6.0]; // planet-count mean mu = t+1
const T5_Q: [f64; 5] = [0.23, 0.26, 0.32, 0.32, 0.90]; // per-planet station prob q_t
const T5_SIGMA: f64 = 2.0;

// erf via Abramowitz & Stegun 7.1.26 (max abs error ~1.5e-7) — plenty for a
// threshold comparison, and avoids pulling in a numerics crate.
fn erf(x: f64) -> f64 {
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * x);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-x * x).exp();
    sign * y
}

// Normal CDF.
fn phi(z: f64) -> f64 {
    0.5 * (1.0 + erf(z / std::f64::consts::SQRT_2))
}

// P(round(N(mu, sigma)) == p), p >= 1.
fn normal_band(p: f64, mu: f64) -> f64 {
    phi((p + 0.5 - mu) / T5_SIGMA) - phi((p - 0.5 - mu) / T5_SIGMA)
}

impl System {
    pub fn is_starter_system(&self) -> bool {
        self.waypoints
            .iter()
            .any(|w| w.waypoint_type == "ENGINEERED_ASTEROID")
    }

    // Posterior probability that this system is generation Tier 5, given its
    // planet count and the number of those planets that have an orbiting station
    // (the rarest tier, and the most valuable to trade). An orbital body shares
    // its parent's coordinates, so a station orbits a planet iff it sits on a
    // planet's (x, y) — no orbit linkage needed. Returns None when there are no
    // planets (the band is undefined for p = 0).
    pub fn p_t5(&self) -> Option<f64> {
        let planet_coords: std::collections::HashSet<(i64, i64)> = self
            .waypoints
            .iter()
            .filter(|w| w.waypoint_type == "PLANET")
            .map(|w| (w.x, w.y))
            .collect();
        let p = planet_coords.len() as i32;
        if p < 1 {
            return None;
        }
        let o = self
            .waypoints
            .iter()
            .filter(|w| w.waypoint_type == "ORBITAL_STATION" && planet_coords.contains(&(w.x, w.y)))
            .count() as i32;

        let terms: [f64; 5] = std::array::from_fn(|i| {
            T5_W[i]
                * normal_band(p as f64, T5_MU[i])
                * T5_Q[i].powi(o)
                * (1.0 - T5_Q[i]).powi(p - o)
        });
        let total: f64 = terms.iter().sum();
        if total <= 0.0 {
            return None;
        }
        Some(terms[4] / total)
    }
}

impl Waypoint {
    pub fn is_market(&self) -> bool {
        if let Some(details) = &self.details
            && details.is_market {
                return true;
            }
        // All jumpgates are also markets
        if self.waypoint_type == "JUMP_GATE" {
            return true;
        }
        false
    }
}
