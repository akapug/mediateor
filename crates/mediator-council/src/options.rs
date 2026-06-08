//! Resolution option generation.
//!
//! Given the `Analysis` from `mediator-core` (which holds the formal crux,
//! ledger findings, and certified-fair `Settlement`s), generate a set of
//! plain-language `ResolutionOption`s the council can vote on.
//!
//! This is deterministic and offline — no LLM calls here. The options
//! are derived from what the formal kernel actually certified.

use mediator_types::{Analysis, Dispute, ResolutionOption, Settlement};

/// Generate resolution options from a dispute and its analysis.
/// Returns a non-empty list.
pub fn generate_options(dispute: &Dispute, analysis: &Analysis) -> Vec<ResolutionOption> {
    let mut options: Vec<ResolutionOption> = Vec::new();
    let mut idx = 0;

    // ── Options from certified fair-division settlements ─────────────────────────────
    for settlement in &analysis.settlements {
        let desc = settlement_description(dispute, settlement, &analysis.ledger_findings);
        options.push(ResolutionOption {
            idx,
            label: format!("Option {}: {}", idx + 1, settlement.label),
            description: desc,
            approval_votes: 0,
            total_votes: 0,
            approved: false,
            settlement: Some(settlement.clone()),
            requires_predicates: crux_predicates(analysis),
        });
        idx += 1;
    }

    // ── Crux-split options: one per world when the crux is undecided ──────────────
    if let Some(crux_text) = &analysis.crux {
        if crux_text.contains("will not") || crux_text.contains("cannot") {
            // There are two refund worlds from the ledger findings.
            let damage_amount = extract_first_amount(&analysis.ledger_findings, true);
            let wear_amount = extract_first_amount(&analysis.ledger_findings, false);

            if let (Some(d), Some(w)) = (damage_amount, wear_amount) {
                if d != w {
                    options.push(ResolutionOption {
                        idx,
                        label: format!("Option {}: If the contested question is resolved YES", idx + 1),
                        description: format!(
                            "If the contested question resolves in the stricter direction: \
                             the refund would be {}. Parties may choose to accept this outcome \
                             and agree that the contested question did apply.",
                            crate::fmt_cents(d)
                        ),
                        approval_votes: 0,
                        total_votes: 0,
                        approved: false,
                        settlement: None,
                        requires_predicates: vec!["crux_holds".to_string()],
                    });
                    idx += 1;

                    options.push(ResolutionOption {
                        idx,
                        label: format!("Option {}: If the contested question is resolved NO", idx + 1),
                        description: format!(
                            "If the contested question resolves in the gentler direction: \
                             the refund would be {}. Parties may choose to accept this outcome \
                             and agree the contested question did not apply.",
                            crate::fmt_cents(w)
                        ),
                        approval_votes: 0,
                        total_votes: 0,
                        approved: false,
                        settlement: None,
                        requires_predicates: vec!["crux_does_not_hold".to_string()],
                    });
                    idx += 1;
                }
            }
        }
    }

    // ── Fallback: full mutual agreement option (always available) ──────────────
    options.push(ResolutionOption {
        idx,
        label: format!("Option {}: Mutual negotiated agreement", idx + 1),
        description: "Both parties agree to a mutually negotiated outcome outside the \
                       formal process. The council recommends this when parties show \
                       sufficient good faith to resolve the contested question directly."
            .to_string(),
        approval_votes: 0,
        total_votes: 0,
        approved: false,
        settlement: None,
        requires_predicates: vec![],
    });

    options
}

fn settlement_description(
    dispute: &Dispute,
    s: &Settlement,
    ledger_findings: &[String],
) -> String {
    let mut parts = Vec::new();

    // Allocations
    for (item_id, party_id) in &s.allocations {
        let item_label = dispute
            .contested_items
            .iter()
            .find(|i| &i.id == item_id)
            .map(|i| i.label.as_str())
            .unwrap_or(item_id.as_str());
        let party_name = dispute
            .parties
            .iter()
            .find(|p| &p.id == party_id)
            .map(|p| p.display_name.as_str())
            .unwrap_or(party_id.as_str());
        parts.push(format!("{item_label} → {party_name}"));
    }

    // Splits
    for (item_id, fraction) in &s.splits {
        let item_label = dispute
            .contested_items
            .iter()
            .find(|i| &i.id == item_id)
            .map(|i| i.label.as_str())
            .unwrap_or(item_id.as_str());
        let pct = (fraction * 100.0).round() as u32;
        let first_party = dispute.parties.first().map(|p| p.display_name.as_str()).unwrap_or("Party A");
        let second_party = dispute.parties.get(1).map(|p| p.display_name.as_str()).unwrap_or("Party B");
        parts.push(format!("{item_label}: {pct}% to {first_party}, {}% to {second_party}", 100 - pct));
    }

    if !ledger_findings.is_empty() {
        parts.push(ledger_findings[0].clone());
    }

    if s.envy_free {
        parts.push("Certified envy-free (neither party prefers the other's share).".to_string());
    }

    if parts.is_empty() {
        s.explanation.clone()
    } else {
        parts.join(" · ")
    }
}

fn crux_predicates(analysis: &Analysis) -> Vec<String> {
    if analysis.crux.is_some() {
        vec!["crux_resolved".to_string()]
    } else {
        vec![]
    }
}

fn extract_first_amount(findings: &[String], damage_world: bool) -> Option<i64> {
    for f in findings {
        let lower = f.to_lowercase();
        let marker = if damage_world { "stricter" } else { "gentler" };
        if lower.contains(marker) || (damage_world && lower.contains("damage")) || (!damage_world && lower.contains("wear")) {
            // Try to extract a dollar amount like "$750.00"
            if let Some(amt) = parse_dollar_amount(f) {
                return Some(amt);
            }
        }
    }
    // Fallback: return first and second dollar amounts found
    let amounts: Vec<i64> = findings
        .iter()
        .flat_map(|f| parse_all_dollar_amounts(f))
        .collect();
    if damage_world { amounts.first().copied() } else { amounts.get(1).copied() }
}

fn parse_dollar_amount(s: &str) -> Option<i64> {
    parse_all_dollar_amounts(s).into_iter().next()
}

fn parse_all_dollar_amounts(s: &str) -> Vec<i64> {
    let mut amounts = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            let mut num = String::new();
            let mut has_dot = false;
            for nc in chars.by_ref() {
                if nc.is_ascii_digit() {
                    num.push(nc);
                } else if nc == '.' && !has_dot {
                    has_dot = true;
                    num.push(nc);
                } else {
                    break;
                }
            }
            if !num.is_empty() {
                if let Ok(f) = num.parse::<f64>() {
                    amounts.push((f * 100.0).round() as i64);
                }
            }
        }
    }
    amounts
}
