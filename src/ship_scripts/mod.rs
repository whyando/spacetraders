pub mod construction;
pub mod exploration;
pub mod logistics;
pub mod mining;
pub mod probe;
pub mod probe_exploration;
pub mod scrap;
pub mod siphon;

use crate::agent_controller::{AgentController, AgentEra};

/// True once the agent has advanced past the home-system build-out (the jump gate
/// is built). The ships that exist only to fund and feed gate construction — the
/// mining + siphon fleet and the dedicated construction hauler — are no longer
/// needed at this point and scrap themselves (see `mining`/`siphon`/`construction`).
///
/// Keyed on era — the single source of truth for fleet phase, and itself defined
/// by the gate finishing — so the scrap cue and the ship-config gate (which also
/// keys on era) can never disagree.
pub fn home_phase_done(ac: &AgentController) -> bool {
    !matches!(
        ac.state().era,
        AgentEra::StartingSystem1 | AgentEra::StartingSystem2
    )
}
