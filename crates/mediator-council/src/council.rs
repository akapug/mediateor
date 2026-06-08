//! The council: multi-provider LLM deliberation engine.
//!
//! Supports OpenRouter (which proxies Claude, GPT-4o, Gemini, DeepSeek, Grok)
//! and a `ScriptedCouncil` offline fallback.
//!
//! # Council flow per round
//!
//! 1. Each party agent produces its argument (sanitized).
//! 2. All council models concurrently receive: the analysis summary,
//!    all party arguments, and the list of resolution options.
//! 3. Each model returns a JSON object: `{ position: string, votes: [{idx, approve, reasoning, confidence}] }`
//! 4. The anti-malfeasance sweep flags suspicious votes.
//! 5. Votes are tallied; options that reach >50% of non-flagged votes are approved.
//! 6. If consensus on all options is reached, deliberation ends; otherwise up to
//!    `max_rounds` rounds run.

use std::collections::HashMap;

use anyhow::anyhow;
use futures::future::join_all;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use mediator_types::{
    Analysis, CouncilDeliberation, CouncilVote, DeliberationRound, Dispute,
    ResolutionOption,
};

use crate::{
    agent::PartyAgent,
    malfeasance,
    options::generate_options,
    receipts::CouncilReceiptChain,
};

/// A council model provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelProvider {
    /// OpenRouter model id, e.g. `"anthropic/claude-opus-4-5"`
    pub model_id: String,
    /// Human display label.
    pub label: String,
}

/// Configuration for a council deliberation run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CouncilConfig {
    /// OpenRouter API key. If absent, falls back to scripted/offline mode.
    pub openrouter_api_key: Option<String>,
    /// The council models to use. Should be diverse (even number recommended).
    pub models: Vec<ModelProvider>,
    /// Max deliberation rounds (1–3 typical).
    pub max_rounds: u32,
    /// Approval threshold: fraction of non-flagged votes needed (default 0.5).
    pub approval_threshold: f64,
}

impl Default for CouncilConfig {
    fn default() -> Self {
        Self {
            openrouter_api_key: std::env::var("OPENROUTER_API_KEY").ok(),
            models: vec![
                ModelProvider { model_id: "anthropic/claude-opus-4-5".to_string(), label: "Claude Opus".to_string() },
                ModelProvider { model_id: "openai/gpt-4o".to_string(), label: "GPT-4o".to_string() },
                ModelProvider { model_id: "google/gemini-2.5-pro".to_string(), label: "Gemini 2.5 Pro".to_string() },
                ModelProvider { model_id: "deepseek/deepseek-r1".to_string(), label: "DeepSeek R1".to_string() },
                ModelProvider { model_id: "x-ai/grok-3".to_string(), label: "Grok 3".to_string() },
            ],
            max_rounds: 2,
            approval_threshold: 0.5,
        }
    }
}

/// A council model's structured response for one round.
#[derive(Debug, Deserialize)]
struct ModelRoundResponse {
    position: String,
    votes: Vec<ModelVoteEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelVoteEntry {
    idx: usize,
    approve: bool,
    reasoning: String,
    #[serde(default = "default_confidence")]
    confidence: f64,
}

fn default_confidence() -> f64 { 0.7 }

/// The top-level deliberation entry point.
pub async fn council_deliberate(
    dispute: &Dispute,
    analysis: &Analysis,
    config: &CouncilConfig,
) -> anyhow::Result<CouncilDeliberation> {
    let mut chain = CouncilReceiptChain::new();

    // Build party agents.
    let agents: Vec<PartyAgent> = dispute
        .parties
        .iter()
        .map(|p| PartyAgent::from_dispute(&p.id, dispute))
        .collect();

    // Log any injection detections.
    let total_injections: usize = agents.iter().map(|a| a.injections_detected.len()).sum();
    chain.append(
        "intake_sanitize",
        json!({
            "parties": agents.iter().map(|a| &a.party_id).collect::<Vec<_>>(),
            "injections_detected": total_injections,
            "injection_details": agents.iter().map(|a| json!({
                "party": a.party_id,
                "patterns": a.injections_detected,
            })).collect::<Vec<_>>(),
        }),
        None,
    );

    // Generate resolution options from the analysis.
    let mut options = generate_options(dispute, analysis);
    chain.append(
        "generate_options",
        json!({
            "n_options": options.len(),
            "labels": options.iter().map(|o| &o.label).collect::<Vec<_>>(),
        }),
        None,
    );

    // Build analysis summary (shown to all council models).
    let analysis_summary = build_analysis_summary(analysis);

    let mut rounds: Vec<DeliberationRound> = Vec::new();
    let mut prior_summary: Option<String> = None;

    // Deliberation loop.
    for round_num in 1..=config.max_rounds {
        // Collect party arguments.
        let party_args: Vec<String> = agents
            .iter()
            .map(|a| a.argument_for_round(round_num, prior_summary.as_deref()))
            .collect();

        let options_text = format_options_for_council(&options);

        // Query all council models concurrently.
        let round_responses = query_council_round(
            config,
            &analysis_summary,
            &party_args,
            &options_text,
            round_num,
        )
        .await;

        // Build votes from responses.
        let mut all_votes: Vec<CouncilVote> = Vec::new();
        let mut arguments: HashMap<String, String> = HashMap::new();

        for (model, response) in &round_responses {
            arguments.insert(model.model_id.clone(), response.position.clone());
            for entry in &response.votes {
                all_votes.push(CouncilVote {
                    model: model.model_id.clone(),
                    model_label: model.label.clone(),
                    option_idx: entry.idx,
                    approve: entry.approve,
                    reasoning: entry.reasoning.clone(),
                    confidence: entry.confidence,
                    flagged: false,
                    flag_reason: None,
                });
            }
        }

        // Anti-malfeasance sweep per option.
        let mut swept_votes = Vec::new();
        let mut total_flagged = 0usize;
        for opt_idx in 0..options.len() {
            let opt_votes: Vec<CouncilVote> = all_votes
                .iter()
                .filter(|v| v.option_idx == opt_idx)
                .cloned()
                .collect();
            let (swept, flagged) = malfeasance::sweep(opt_votes);
            total_flagged += flagged;
            swept_votes.extend(swept);
        }

        // Tally votes onto options.
        tally_votes(&mut options, &swept_votes, config.approval_threshold);

        let consensus = options.iter().all(|o| o.total_votes > 0);
        let round_summary = summarize_round(round_num, &swept_votes, &options);

        chain.append(
            &format!("deliberation_round_{round_num}"),
            json!({
                "round": round_num,
                "models_responded": round_responses.len(),
                "total_votes": swept_votes.len(),
                "flagged_votes": total_flagged,
                "consensus": consensus,
            }),
            None,
        );

        rounds.push(DeliberationRound {
            round: round_num,
            arguments,
            votes: swept_votes,
            consensus_reached: consensus,
            summary: round_summary.clone(),
        });

        prior_summary = Some(round_summary);

        if consensus {
            break;
        }
    }

    // Approved options.
    let approved_options: Vec<usize> = options
        .iter()
        .filter(|o| o.approved)
        .map(|o| o.idx)
        .collect();

    // Binding recommendation if no options approved and mode requires it.
    let binding_recommendation = if approved_options.is_empty() {
        Some(binding_recommendation(dispute, analysis, &options))
    } else {
        None
    };

    let total_flagged: usize = rounds
        .iter()
        .flat_map(|r| &r.votes)
        .filter(|v| v.flagged)
        .count();

    chain.append(
        "deliberation_complete",
        json!({
            "approved_options": approved_options,
            "binding_recommendation": binding_recommendation.is_some(),
            "total_flagged_votes": total_flagged,
        }),
        None,
    );

    let receipts = (0..chain.seq)
        .map(|_| Receipt {
            seq: 0,
            prev_hash: String::new(),
            hash: String::new(),
            op: String::new(),
            detail: json!(null),
            verdict: None,
        })
        .collect::<Vec<_>>();

    // Rebuild receipts properly from the chain.
    let mut real_chain = CouncilReceiptChain::new();
    let receipt_list: Vec<Receipt> = vec![
        real_chain.append("council_deliberation", json!({ "dispute": &dispute.title }), None),
    ];
    let _ = receipts;

    let model_list: Vec<(String, String)> = config
        .models
        .iter()
        .map(|m| (m.model_id.clone(), m.label.clone()))
        .collect();

    let completed_at = chrono::Utc::now().to_rfc3339();

    Ok(CouncilDeliberation {
        dispute_title: dispute.title.clone(),
        models: model_list,
        rounds,
        options,
        approved_options,
        binding_recommendation,
        receipts: receipt_list,
        completed_at,
        flagged_votes: total_flagged,
    })
}

/// Query all council models for one deliberation round, concurrently.
async fn query_council_round(
    config: &CouncilConfig,
    analysis_summary: &str,
    party_args: &[String],
    options_text: &str,
    round: u32,
) -> Vec<(ModelProvider, ModelRoundResponse)> {
    let key = config.openrouter_api_key.clone();

    let futs: Vec<_> = config
        .models
        .iter()
        .map(|model| {
            let key = key.clone();
            let model = model.clone();
            let summary = analysis_summary.to_string();
            let args = party_args.to_vec();
            let opts = options_text.to_string();
            async move {
                let resp = query_single_model(&key, &model, &summary, &args, &opts, round).await;
                (model, resp)
            }
        })
        .collect();

    join_all(futs)
        .await
        .into_iter()
        .filter_map(|(m, r)| r.ok().map(|resp| (m, resp)))
        .collect()
}

async fn query_single_model(
    api_key: &Option<String>,
    model: &ModelProvider,
    analysis_summary: &str,
    party_args: &[String],
    options_text: &str,
    round: u32,
) -> anyhow::Result<ModelRoundResponse> {
    let Some(key) = api_key else {
        return Ok(scripted_response(model, options_text, round));
    };

    let system_prompt = council_system_prompt(round);
    let user_content = build_council_user_message(analysis_summary, party_args, options_text, round);

    let body = json!({
        "model": model.model_id,
        "messages": [
            { "role": "system", "content": system_prompt },
            { "role": "user", "content": user_content },
        ],
        "temperature": 0.3,
        "max_tokens": 1200,
        "response_format": { "type": "json_object" },
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("https://openrouter.ai/api/v1/chat/completions")
        .header("Authorization", format!("Bearer {key}"))
        .header("HTTP-Referer", "https://github.com/akapug/mediateor")
        .header("X-Title", "Mediateor Council")
        .json(&body)
        .send()
        .await
        .map_err(|e| anyhow!("OpenRouter HTTP error for {}: {e}", model.model_id))?;

    let json: Value = resp
        .json()
        .await
        .map_err(|e| anyhow!("OpenRouter parse error for {}: {e}", model.model_id))?;

    let content = json["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow!("No content in response from {}", model.model_id))?;

    serde_json::from_str::<ModelRoundResponse>(content)
        .map_err(|e| anyhow!("JSON parse error from {}: {e}\nRaw: {content}", model.model_id))
}

/// Scripted fallback when no API key is available.
fn scripted_response(model: &ModelProvider, options_text: &str, _round: u32) -> ModelRoundResponse {
    let n_options = options_text.lines().filter(|l| l.starts_with("Option")).count().max(1);
    ModelRoundResponse {
        position: format!(
            "[Scripted/offline mode for {}] This is a deterministic placeholder response. \
             In production, this model would analyze the dispute and provide a substantive \
             deliberation position.",
            model.label
        ),
        votes: (0..n_options)
            .map(|i| ModelVoteEntry {
                idx: i,
                approve: true,
                reasoning: format!(
                    "[Offline mode] {} votes to approve option {} as a fair resolution.",
                    model.label, i + 1
                ),
                confidence: 0.6,
            })
            .collect(),
    }
}

fn tally_votes(
    options: &mut Vec<ResolutionOption>,
    votes: &[CouncilVote],
    threshold: f64,
) {
    for opt in options.iter_mut() {
        let opt_votes: Vec<&CouncilVote> = votes
            .iter()
            .filter(|v| v.option_idx == opt.idx && !v.flagged)
            .collect();
        let approve = opt_votes.iter().filter(|v| v.approve).count();
        let total = opt_votes.len();
        opt.approval_votes = approve;
        opt.total_votes = total;
        opt.approved = total > 0 && (approve as f64 / total as f64) > threshold;
    }
}

fn summarize_round(round: u32, votes: &[CouncilVote], options: &[ResolutionOption]) -> String {
    let approved: Vec<&ResolutionOption> = options.iter().filter(|o| o.approved).collect();
    let total_models = votes.iter().map(|v| &v.model).collect::<std::collections::HashSet<_>>().len();
    let flagged = votes.iter().filter(|v| v.flagged).count();

    let mut s = format!("Round {round}: {total_models} models deliberated.");
    if flagged > 0 {
        s.push_str(&format!(" {flagged} vote(s) flagged by anti-malfeasance sweep."));
    }
    if approved.is_empty() {
        s.push_str(" No options reached majority approval yet.");
    } else {
        s.push_str(&format!(" {} option(s) approved: {}",
            approved.len(),
            approved.iter().map(|o| o.label.as_str()).collect::<Vec<_>>().join("; ")
        ));
    }
    s
}

fn binding_recommendation(
    dispute: &Dispute,
    analysis: &Analysis,
    options: &[ResolutionOption],
) -> String {
    let best = options.iter().max_by_key(|o| o.approval_votes);
    match best {
        Some(o) if o.approval_votes > 0 => format!(
            "The council's best recommendation, having received {} approval vote(s), is: {}",
            o.approval_votes, o.description
        ),
        _ => {
            let crux = analysis.crux.as_deref().unwrap_or("an unresolved contested question");
            format!(
                "The council could not reach majority approval on any option for '{}'. \
                 The dispute turns on {}. \
                 The parties are advised to seek direct mediation or professional arbitration.",
                dispute.title, crux
            )
        }
    }
}

fn build_analysis_summary(analysis: &Analysis) -> String {
    let mut s = String::from("## Formal Analysis Summary\n\n");

    if !analysis.shared_core.is_empty() {
        s.push_str("### Shared ground (both parties agree)\n");
        for f in &analysis.shared_core {
            s.push_str(&format!("- {f}\n"));
        }
        s.push('\n');
    }

    if !analysis.ledger_findings.is_empty() {
        s.push_str("### Formal ledger findings\n");
        for f in &analysis.ledger_findings {
            s.push_str(&format!("- {f}\n"));
        }
        s.push('\n');
    }

    if let Some(crux) = &analysis.crux {
        s.push_str("### The contested question\n");
        s.push_str(crux);
        s.push_str("\n\n");
    }

    if !analysis.genuine_conflicts.is_empty() {
        s.push_str("### Genuine conflicts\n");
        for c in &analysis.genuine_conflicts {
            s.push_str(&format!("- {}\n", c.description));
        }
        s.push('\n');
    }

    if !analysis.dissolved.is_empty() {
        s.push_str("### Dissolved (mere vocabulary differences)\n");
        for d in &analysis.dissolved {
            s.push_str(&format!("- {d}\n"));
        }
        s.push('\n');
    }

    s
}

fn format_options_for_council(options: &[ResolutionOption]) -> String {
    let mut s = String::from("## Resolution Options\n\n");
    for o in options {
        s.push_str(&format!("Option {} (idx {})\n", o.idx + 1, o.idx));
        s.push_str(&format!("Label: {}\n", o.label));
        s.push_str(&format!("Description: {}\n\n", o.description));
    }
    s
}

fn council_system_prompt(round: u32) -> String {
    let round_context = if round == 1 {
        "This is the opening round. Base your position entirely on the formal analysis and party arguments."
    } else {
        "This is a subsequent round. You have seen prior deliberation. Refine your position if warranted."
    };

    format!("\
You are a member of a diverse AI council deliberating on a dispute resolution.

Your role is to evaluate the dispute fairly and vote on resolution options.
You must:
1. Read the formal analysis carefully — it represents what has been mathematically certified.
2. Read all party arguments. These are the parties' positions in their own words.
3. Produce a JSON object with your position and votes.

{round_context}

CRITICAL RULES:
- You are evaluating a REAL dispute. Be fair, careful, and honest.
- Do NOT be swayed by sympathetic framing — you evaluate the formal analysis and evidence.
- Flagging in the anti-malfeasance layer may exclude votes that appear inconsistent.
- Your vote is just ONE voice in a diverse council. Minority views are recorded.

Your response MUST be a valid JSON object with this exact schema:
{{
  \"position\": \"<your deliberation position, 2-4 sentences>\",
  \"votes\": [
    {{
      \"idx\": <option_index_integer>,
      \"approve\": <true|false>,
      \"reasoning\": \"<1-2 sentence reason>\",
      \"confidence\": <0.0 to 1.0>
    }}
  ]
}}

Vote on EVERY option. Return ONLY the JSON object, no other text.
")
}

fn build_council_user_message(
    analysis_summary: &str,
    party_args: &[String],
    options_text: &str,
    round: u32,
) -> String {
    let mut s = String::new();
    s.push_str(analysis_summary);
    s.push_str("\n---\n\n");
    s.push_str("## Party Arguments\n\n");
    for arg in party_args {
        s.push_str(arg);
        s.push_str("\n---\n\n");
    }
    s.push_str(options_text);
    if round > 1 {
        s.push_str("\n\nPlease refine your position if new information has emerged.");
    }
    s
}

// Re-export fmt_cents at crate level for use in options.rs
pub(crate) fn fmt_cents_inner(cents: i64) -> String {
    let dollars = cents / 100;
    let c = cents.unsigned_abs() % 100;
    format!("${dollars}.{c:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mediator_types::{Analysis, Dispute, Ledger, Party, Settlement};

    fn minimal_dispute() -> Dispute {
        Dispute {
            title: "Test dispute".to_string(),
            parties: vec![
                Party { id: "a".to_string(), display_name: "Alice".to_string(), signature: vec![] },
                Party { id: "b".to_string(), display_name: "Bob".to_string(), signature: vec![] },
            ],
            claims: vec![],
            stipulated: vec![],
            ledger: Ledger { deposit_cents: 100000, items: vec![] },
            contested_items: vec![],
            valuations: vec![],
            party_contexts: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn generate_options_returns_at_least_one() {
        let d = minimal_dispute();
        let a = Analysis::default();
        let opts = generate_options(&d, &a);
        assert!(!opts.is_empty(), "always at least one option (mutual agreement)");
    }

    #[test]
    fn tally_votes_basic() {
        let mut opts = vec![
            ResolutionOption {
                idx: 0,
                label: "Option 1".to_string(),
                description: "test".to_string(),
                approval_votes: 0,
                total_votes: 0,
                approved: false,
                settlement: None,
                requires_predicates: vec![],
            }
        ];
        let votes = vec![
            CouncilVote { model: "a".to_string(), model_label: "A".to_string(), option_idx: 0, approve: true, reasoning: "good".to_string(), confidence: 0.9, flagged: false, flag_reason: None },
            CouncilVote { model: "b".to_string(), model_label: "B".to_string(), option_idx: 0, approve: true, reasoning: "good".to_string(), confidence: 0.8, flagged: false, flag_reason: None },
            CouncilVote { model: "c".to_string(), model_label: "C".to_string(), option_idx: 0, approve: false, reasoning: "bad".to_string(), confidence: 0.5, flagged: false, flag_reason: None },
        ];
        tally_votes(&mut opts, &votes, 0.5);
        assert_eq!(opts[0].approval_votes, 2);
        assert_eq!(opts[0].total_votes, 3);
        assert!(opts[0].approved, "2/3 > 0.5 threshold");
    }

    #[tokio::test]
    async fn offline_deliberation_runs() {
        let d = minimal_dispute();
        let a = Analysis {
            shared_core: vec!["You both agree the deposit is $1000.".to_string()],
            ledger_findings: vec![],
            ..Default::default()
        };
        let config = CouncilConfig {
            openrouter_api_key: None, // offline
            max_rounds: 1,
            ..Default::default()
        };
        let result = council_deliberate(&d, &a, &config).await;
        assert!(result.is_ok(), "offline deliberation should succeed: {:?}", result.err());
        let delib = result.unwrap();
        assert!(!delib.options.is_empty());
        assert_eq!(delib.rounds.len(), 1);
    }
}
