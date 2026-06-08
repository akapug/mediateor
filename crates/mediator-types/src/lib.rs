//! `mediator-types` — the shared contract for the trusted-mediator kernel.
//!
//! Pure data + trait interfaces. **No logic.** Every other crate depends on
//! this and only this for cross-crate types.
//!
//! Design north star: a *backstage cathedral*. The host prover holds the
//! formalizable core; the council operates it; humans receive a kind, plain
//! rendering. Money is always integer **cents** (never floats).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type PartyId = String;
pub type ClaimId = String;
pub type ItemId = String;
pub type ModelId = String;

// ───────────────────────── typed many-sorted IR ─────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Sort {
    Bool,
    Int,
    Real,
    Uninterp(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Term {
    Var(String),
    IntLit(i64),
    /// Function/predicate/constant application. Nullary `args` = a constant.
    App(String, Vec<Term>),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Formula {
    Atom(Term),
    Eq(Term, Term),
    Le(Term, Term),
    Lt(Term, Term),
    Not(Box<Formula>),
    And(Vec<Formula>),
    Or(Vec<Formula>),
    Implies(Box<Formula>, Box<Formula>),
    Iff(Box<Formula>, Box<Formula>),
    Forall(String, Sort, Box<Formula>),
    Exists(String, Sort, Box<Formula>),
    Obligation(Box<Formula>),
    Permission(Box<Formula>),
}

/// An ontology/signature symbol a party brings to the table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sig {
    pub name: String,
    pub arg_sorts: Vec<Sort>,
    pub ret: Sort,
    pub gloss: String,
}

// ───────────────────────────── the dispute ──────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub id: ClaimId,
    pub party: PartyId,
    pub nl: String,
    pub formula: Formula,
    pub english_render: String,
    pub weight: i64,
    pub defeasible: bool,
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LedgerItem {
    pub id: ItemId,
    pub label: String,
    pub amount_cents: i64,
    pub asserted_by: PartyId,
    pub disputed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    pub deposit_cents: i64,
    pub items: Vec<LedgerItem>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContestedItem {
    pub id: ItemId,
    pub label: String,
    pub divisible: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Valuation {
    pub party: PartyId,
    pub item: ItemId,
    pub points: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Party {
    pub id: PartyId,
    pub display_name: String,
    pub signature: Vec<Sig>,
}

/// Extended party context: everything a party's agent carries into the council.
/// This is the intake form — filed before the council convenes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct PartyContext {
    /// Plain-language narrative this party wants the council to understand.
    pub narrative: String,
    /// Key evidence items (filenames, descriptions, or free text).
    pub evidence: Vec<EvidenceItem>,
    /// Pre-agreed outcome preferences, ordered by priority.
    pub priorities: Vec<String>,
    /// Any constraints the party insists must hold in any resolution.
    pub hard_constraints: Vec<String>,
    /// Optional: how the party pre-agreed to handle a final-decision outcome.
    /// E.g. "binding arbitration", "accept council majority", "mediated proposal only".
    pub final_decision_mode: Option<String>,
}

/// A single piece of evidence a party submits.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvidenceItem {
    pub id: String,
    pub label: String,
    /// Free-text description or inline content. URLs/hashes accepted.
    pub content: String,
    /// Which party submitted this.
    pub submitted_by: PartyId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dispute {
    pub title: String,
    pub parties: Vec<Party>,
    pub claims: Vec<Claim>,
    pub stipulated: Vec<Formula>,
    pub ledger: Ledger,
    pub contested_items: Vec<ContestedItem>,
    pub valuations: Vec<Valuation>,
    /// Rich per-party intake context. Optional — absent in legacy scenarios.
    #[serde(default)]
    pub party_contexts: HashMap<PartyId, PartyContext>,
}

// ─────────────────────────── prover verdicts ────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Verdict {
    Proved,
    Refuted,
    Unknown,
    Error(String),
}

// ───────────────────────────── settlements ──────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settlement {
    pub label: String,
    pub allocations: Vec<(ItemId, PartyId)>,
    pub splits: Vec<(ItemId, f64)>,
    pub party_points: Vec<(PartyId, f64)>,
    pub envy_free: bool,
    pub equitable: bool,
    pub pareto_optimal: bool,
    pub explanation: String,
}

// ─────────────────── receipts: append-only, hash-chained ─────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub seq: u64,
    pub prev_hash: String,
    pub hash: String,
    pub op: String,
    pub detail: serde_json::Value,
    pub verdict: Option<Verdict>,
}

// ──────────────── the analysis: what both UX views render ────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conflict {
    pub description: String,
    pub parties: Vec<PartyId>,
    pub claim_ids: Vec<ClaimId>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Analysis {
    pub shared_core: Vec<String>,
    pub genuine_conflicts: Vec<Conflict>,
    pub dissolved: Vec<String>,
    pub ledger_refund_cents: Option<i64>,
    pub ledger_findings: Vec<String>,
    pub crux: Option<String>,
    pub settlements: Vec<Settlement>,
}

// ─────────────────────── council deliberation types ──────────────────────

/// One model's vote on a resolution option.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CouncilVote {
    /// The model that cast this vote.
    pub model: ModelId,
    /// Friendly display label for the model.
    pub model_label: String,
    /// The option index being voted on.
    pub option_idx: usize,
    /// true = approve, false = reject.
    pub approve: bool,
    /// The model's reasoning (kept for transparency; not authoritative).
    pub reasoning: String,
    /// Confidence 0.0–1.0 the model expresses.
    pub confidence: f64,
    /// Did the anti-malfeasance check flag this vote as suspect?
    pub flagged: bool,
    /// If flagged, a description of why.
    pub flag_reason: Option<String>,
}

/// One round of the council deliberation: all models produce arguments,
/// then vote.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeliberationRound {
    pub round: u32,
    /// Per-model argument/position text (model_id → text).
    pub arguments: HashMap<ModelId, String>,
    /// All votes cast this round.
    pub votes: Vec<CouncilVote>,
    /// Did consensus emerge this round?
    pub consensus_reached: bool,
    /// Summary of this round's outcome (generated by the synthesizer).
    pub summary: String,
}

/// A resolution option proposed by the council for parties to act on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResolutionOption {
    pub idx: usize,
    pub label: String,
    /// Plain-English description of the resolution.
    pub description: String,
    /// The number of council models that approved this option.
    pub approval_votes: usize,
    /// Total valid votes cast.
    pub total_votes: usize,
    /// true when approval_votes > total_votes / 2.
    pub approved: bool,
    /// Settlement detail if this maps to a formal fair-division outcome.
    pub settlement: Option<Settlement>,
    /// The normalized formal predicate(s) this resolution requires to hold.
    /// Empty for non-formalizable options.
    pub requires_predicates: Vec<String>,
}

/// The full council deliberation record for a dispute.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CouncilDeliberation {
    /// The dispute this deliberation is over.
    pub dispute_title: String,
    /// Council models that participated.
    pub models: Vec<(ModelId, String)>,
    /// All rounds of deliberation (typically 1–3).
    pub rounds: Vec<DeliberationRound>,
    /// Resolution options the council generated and voted on.
    pub options: Vec<ResolutionOption>,
    /// Options that passed the majority vote threshold (> 50% approval).
    pub approved_options: Vec<usize>,
    /// If no options were approved and parties pre-agreed to binding mode,
    /// the council's single best recommendation.
    pub binding_recommendation: Option<String>,
    /// Hash-chained receipts covering the full deliberation.
    pub receipts: Vec<Receipt>,
    /// UTC ISO-8601 timestamp when deliberation completed.
    pub completed_at: String,
    /// anti-malfeasance: total suspicious votes detected and excluded.
    pub flagged_votes: usize,
}

// ──────────────────────── trait interfaces (seams) ───────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Obligation {
    pub name: String,
    pub goal: String,
    pub proof: String,
}

pub trait Prover {
    fn check(&self, preamble: &str, obligations: &[Obligation]) -> HashMap<String, Verdict>;
}

pub trait FairDivider {
    fn divide(
        &self,
        items: &[ContestedItem],
        valuations: &[Valuation],
        parties: &[PartyId],
    ) -> Vec<Settlement>;
}

pub trait LlmOperator {
    fn formalize(&self, nl: &str, sig: &[Sig]) -> Result<Formula, String>;
    fn render_english(&self, f: &Formula) -> String;
}
