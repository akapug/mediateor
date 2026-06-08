//! `mediator-council` — multi-provider LLM council for dispute resolution.
//!
//! # Architecture
//!
//! Each party has a **PartyAgent** that holds the party's full context
//! (narrative, evidence, claims, priorities) and produces an *argument* for
//! each deliberation round. The argument is sanitized before council models
//! see it (prompt-injection scrubbing).
//!
//! A diverse **council** of models (Claude, GPT-4o, Gemini, DeepSeek, Grok…)
//! each independently reads the sanitized arguments and the `Analysis` from
//! `mediator-core`, then: (a) produces its own deliberation position and
//! (b) votes approve/reject on each resolution option.
//!
//! The anti-malfeasance layer flags votes that look like outlier injection:
//! a vote is suspicious when it is the *sole* outlier and the model's
//! stated reasoning contradicts its vote direction.
//!
//! # Trust boundary
//!
//! - Party input is sandboxed through `sanitize_input()` before any model sees it.
//! - Model output is parsed structurally; prose reasoning is advisory only.
//! - Vote tallying uses the *normalized* option index, not prose.
//! - The receipt chain covers the full deliberation.
//!
//! # Offline / scripted mode
//!
//! When `MEDIATEOR_COUNCIL_OFFLINE=1` or no API key is present, the crate
//! falls back to `ScriptedCouncil` which produces deterministic results
//! suitable for demos and tests.

pub mod agent;
pub mod council;
pub mod malfeasance;
pub mod options;
pub mod receipts;

pub use agent::PartyAgent;
pub use council::{Council, CouncilConfig, ModelProvider, council_deliberate};
pub use options::generate_options;

use mediator_types::{
    Analysis, CouncilDeliberation, CouncilVote, DeliberationRound, Dispute,
    ResolutionOption, Receipt,
};

/// Run the full council deliberation pipeline:
///   1. Build party agents from dispute + contexts
///   2. Generate resolution options from `Analysis`
///   3. Run council rounds (arg phase + vote phase)
///   4. Anti-malfeasance sweep
///   5. Tally votes, mark approved options
///   6. Assemble `CouncilDeliberation` with hash-chained receipts
pub async fn deliberate(
    dispute: &Dispute,
    analysis: &Analysis,
    config: &CouncilConfig,
) -> anyhow::Result<CouncilDeliberation> {
    council::council_deliberate(dispute, analysis, config).await
}
