//! Pretty terminal output. Pure formatting — takes core types, returns Strings.
//! Kept apart from main so the command dispatch stays about *what*, not *how it
//! looks*. No color crate yet (boring stdlib formatting); easy to add later.

use org_core::{Inference, Node, Person, SearchHit};

/// One person as a multi-line detail block.
pub fn person_detail(p: &Person) -> String {
    let mut out = format!("#{}  {}\n", p.id, p.name);
    push_field(&mut out, "Team", p.team.as_deref());
    push_field(&mut out, "Title", p.title.as_deref());
    push_field(&mut out, "Notes", p.notes.as_deref());
    out
}

/// A `Label: value` line, skipped entirely when the value is absent.
fn push_field(out: &mut String, label: &str, value: Option<&str>) {
    if let Some(v) = value {
        // 6-wide right-pad so the colons line up for the short labels we use.
        out.push_str(&format!("  {label:<6} {v}\n"));
    }
}

/// A flat list of people as a simple aligned table: id, name, team, title.
pub fn person_table(people: &[Person]) -> String {
    if people.is_empty() {
        return "(no people)\n".to_string();
    }
    // Width the name column to the longest name for tidy alignment.
    let name_w = people.iter().map(|p| p.name.len()).max().unwrap_or(4).max(4);
    let mut out = String::new();
    for p in people {
        out.push_str(&format!(
            "  {:>4}  {:<name_w$}  {:<28}  {}\n",
            p.id,
            p.name,
            p.team.as_deref().unwrap_or("-"),
            p.title.as_deref().unwrap_or("-"),
            name_w = name_w,
        ));
    }
    out
}

/// Search results, strongest first. Score is shown dimmed in brackets so the
/// user can see why ordering happened without it being noisy.
pub fn search_results(hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return "(no matches)\n".to_string();
    }
    let mut out = String::new();
    for h in hits {
        let p = &h.person;
        out.push_str(&format!(
            "  #{:<4} {:<20} {}  {}\n",
            p.id,
            p.name,
            p.team.as_deref().unwrap_or("-"),
            p.title.as_deref().unwrap_or(""),
        ));
    }
    out
}

/// Team headcounts as aligned `count  team` rows.
pub fn teams(counts: &[(String, i64)]) -> String {
    if counts.is_empty() {
        return "(no teams)\n".to_string();
    }
    let mut out = String::new();
    for (team, n) in counts {
        out.push_str(&format!("  {n:>3}  {team}\n"));
    }
    out
}

/// Chain of command: nearest boss first, as "→ Name" lines with depth indent.
pub fn chain(nodes: &[Node]) -> String {
    if nodes.is_empty() {
        return "  (reports to no one — this is a root)\n".to_string();
    }
    let mut out = String::new();
    for n in nodes {
        for _ in 1..n.depth {
            out.push_str("  ");
        }
        out.push_str(&format!("  → {}\n", n.name));
    }
    out
}

/// Report the result of a boss inference in plain language.
pub fn inference(person_name: &str, inf: &Inference) -> String {
    match inf {
        Inference::Inferred {
            votes,
            total,
            confidence,
            ..
        } => format!(
            "Inferred a boss for {person_name} ({votes}/{total} teammates agree, \
             confidence {:.2}).\n",
            confidence
        ),
        Inference::NoGuess { reason } => {
            format!("No boss inferred for {person_name}: {reason}.\n")
        }
    }
}
