//! Fuzzy search across people.
//!
//! Two ways a person matches: (1) case-insensitive substring on any field, or
//! (2) Levenshtein typo-tolerance against words in name/team/title (budget
//! scales with query length). Results rank by *where* the match landed:
//! name > team > title > notes. We load all people and filter in Rust — a
//! directory is small, and this keeps the matching logic (which AI will later
//! extend) out of SQL.

use crate::db::Db;
use crate::model::{Person, Result};
use crate::people::list_people;

/// A person plus why/how strongly they matched. Higher `score` ranks first.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub person: Person,
    /// Field-weighted score; ties broken by name. Opaque, only for ordering.
    pub score: i32,
}

/// Field weights: a name hit beats a team hit beats a title hit beats notes.
/// Spaced out so an exact/substring bonus can't let a lower field leapfrog.
const W_NAME: i32 = 1000;
const W_TEAM: i32 = 100;
const W_TITLE: i32 = 10;
const W_NOTES: i32 = 1;

/// Levenshtein budget: how many typos we tolerate, scaling with query length.
/// 0 for very short queries (too ambiguous), then ~1 per 4 chars, capped at 3.
fn typo_budget(query: &str) -> usize {
    match query.chars().count() {
        0..=2 => 0,
        3..=4 => 1,
        5..=8 => 2,
        _ => 3,
    }
}

/// Case-insensitive fuzzy search. Empty query returns everyone (name order).
pub async fn fuzzy_search(db: &Db, query: &str) -> Result<Vec<SearchHit>> {
    let people = list_people(db, None).await?;
    let q = query.trim().to_lowercase();

    if q.is_empty() {
        return Ok(people
            .into_iter()
            .map(|person| SearchHit { person, score: 0 })
            .collect());
    }

    let budget = typo_budget(&q);
    let mut hits: Vec<SearchHit> = people
        .into_iter()
        .filter_map(|person| score_person(&person, &q, budget).map(|score| SearchHit { person, score }))
        .collect();

    // Sort by score desc, then name asc for stable, readable output.
    hits.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| a.person.name.cmp(&b.person.name))
    });
    Ok(hits)
}

/// How a selector resolved. Distinct outcomes rather than an error so the CLI
/// can format each case (a candidate list for Ambiguous) instead of parsing
/// error strings.
#[derive(Debug, Clone)]
pub enum Resolution {
    One(Person),
    /// Multiple people matched by name — the caller should show these and ask
    /// for an id. Ordered as fuzzy_search returned them (strongest first).
    Ambiguous(Vec<Person>),
    NotFound,
}

/// Resolve a CLI selector — an id like "5" or a name fragment like "trent" —
/// to a person.
///
/// An all-digits selector is *always* an id: a miss is NotFound, never a
/// fall-through to name search (nobody should reach person #23 by being the
/// unique name-match for "23"). Anything else is matched against *names only*,
/// with fuzzy_search's typo tolerance; matches on team/title/notes are
/// ignored — resolving `who ids` to somebody via their team would surprise.
pub async fn resolve_person(db: &Db, selector: &str) -> Result<Resolution> {
    let s = selector.trim();

    if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
        // parse can still fail on overflow (e.g. 25 digits) — treat as no such id.
        let Ok(id) = s.parse::<i64>() else {
            return Ok(Resolution::NotFound);
        };
        return Ok(match crate::people::get_person(db, id).await? {
            Some(p) => Resolution::One(p),
            None => Resolution::NotFound,
        });
    }

    // Name-field hits only: fuzzy_search scores name matches at >= W_NAME and
    // every other field strictly below it (that's what the weight spacing is for).
    let candidates: Vec<Person> = fuzzy_search(db, s)
        .await?
        .into_iter()
        .filter(|h| h.score >= W_NAME)
        .map(|h| h.person)
        .collect();

    Ok(match candidates.len() {
        0 => Resolution::NotFound,
        1 => Resolution::One(candidates.into_iter().next().expect("len checked")),
        _ => Resolution::Ambiguous(candidates),
    })
}

/// Score one person against the lowercased query, or None if no field matches.
/// We take the single best field weight (not a sum) so ranking is purely "what
/// is the strongest place this matched", then add a small bonus for substring
/// (exact-ish) over a mere fuzzy word match.
fn score_person(person: &Person, q: &str, budget: usize) -> Option<i32> {
    let fields: [(i32, Option<&str>); 4] = [
        (W_NAME, Some(person.name.as_str())),
        (W_TEAM, person.team.as_deref()),
        (W_TITLE, person.title.as_deref()),
        (W_NOTES, person.notes.as_deref()),
    ];

    let mut best: Option<i32> = None;
    for (weight, field) in fields {
        let Some(text) = field else { continue };
        if let Some(bonus) = field_match(text, q, budget) {
            let s = weight + bonus;
            best = Some(best.map_or(s, |b| b.max(s)));
        }
    }
    best
}

/// Does `q` match `text`? Returns a small bonus on match (substring beats
/// fuzzy), or None. Notes are substring-only — fuzzy over long free text is
/// noisy and fuzzy budget is meant for short identifier-like fields.
fn field_match(text: &str, q: &str, budget: usize) -> Option<i32> {
    let lower = text.to_lowercase();

    // Substring match (case-insensitive) — the strong signal.
    if lower.contains(q) {
        return Some(5);
    }

    // Fuzzy: compare the query against each word in the field AND the whole
    // field. Per-word catches "treny" -> "Trent"; whole-field catches a typo in
    // a multi-word query ("trent rusg" -> "Trent Rush"). Cheap on short fields.
    if budget > 0 {
        if strsim::levenshtein(&lower, q) <= budget {
            return Some(0);
        }
        for word in lower.split_whitespace() {
            if strsim::levenshtein(word, q) <= budget {
                return Some(0);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::{add_person, PersonInput};

    async fn seed(db: &Db) {
        let people = [
            ("Trent Rush", "IDS Fulfillment", "Sr Engineer", Some("rust and sql")),
            ("Dana Cruz", "Marketing Delivery Tracking", "Director", None),
            ("Jane Doe", "IDS Fulfillment", "SWE II", Some("mentored by trent")),
        ];
        for (name, team, title, notes) in people {
            add_person(
                db,
                PersonInput {
                    name: name.to_string(),
                    team: Some(team.to_string()),
                    title: Some(title.to_string()),
                    notes: notes.map(|n| n.to_string()),
                },
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn substring_case_insensitive() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        let hits = fuzzy_search(&db, "TRENT").await.unwrap();
        // Trent matches on name (strong); Jane matches only in notes (weak).
        assert_eq!(hits[0].person.name, "Trent Rush");
        assert!(hits.iter().any(|h| h.person.name == "Jane Doe"));
        // Name hit outranks the notes hit.
        assert!(hits[0].score > hits.last().unwrap().score);
    }

    #[tokio::test]
    async fn typo_tolerance_finds_name() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        // "treny" is one edit from "trent" — within budget for a 5-char query.
        let hits = fuzzy_search(&db, "treny").await.unwrap();
        assert!(hits.iter().any(|h| h.person.name == "Trent Rush"));
    }

    #[tokio::test]
    async fn ranks_name_over_team() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        // "IDS" matches team for Trent and Jane; nobody has it in their name.
        let hits = fuzzy_search(&db, "IDS").await.unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.score < W_NAME)); // team-level, not name
    }

    #[tokio::test]
    async fn empty_query_returns_all() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        assert_eq!(fuzzy_search(&db, "  ").await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn no_match_returns_empty() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        assert!(fuzzy_search(&db, "zzzzqqqq").await.unwrap().is_empty());
    }

    // --- resolve_person ------------------------------------------------------

    #[tokio::test]
    async fn resolve_digits_is_id_lookup_only() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        // Id 1 exists (first insert).
        match resolve_person(&db, "1").await.unwrap() {
            Resolution::One(p) => assert_eq!(p.id, 1),
            other => panic!("expected One, got {other:?}"),
        }
        // Digits never fall through to name search.
        assert!(matches!(
            resolve_person(&db, "999").await.unwrap(),
            Resolution::NotFound
        ));
        // Absurdly long digit strings (i64 overflow) are NotFound, not a panic.
        assert!(matches!(
            resolve_person(&db, "99999999999999999999999999").await.unwrap(),
            Resolution::NotFound
        ));
    }

    #[tokio::test]
    async fn resolve_unique_name_even_with_typo() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        match resolve_person(&db, "trent").await.unwrap() {
            Resolution::One(p) => assert_eq!(p.name, "Trent Rush"),
            other => panic!("expected One, got {other:?}"),
        }
        // One edit away still resolves.
        match resolve_person(&db, "treny").await.unwrap() {
            Resolution::One(p) => assert_eq!(p.name, "Trent Rush"),
            other => panic!("expected One, got {other:?}"),
        }
        // A typo in a multi-word selector matches against the whole name.
        match resolve_person(&db, "trent rusg").await.unwrap() {
            Resolution::One(p) => assert_eq!(p.name, "Trent Rush"),
            other => panic!("expected One, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_ignores_non_name_matches() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        // "IDS" matches two people by team, zero by name -> NotFound, not
        // Ambiguous: selectors resolve by name only.
        assert!(matches!(
            resolve_person(&db, "IDS").await.unwrap(),
            Resolution::NotFound
        ));
        // Jane has "trent" in her NOTES; that must not make "trent" ambiguous.
        match resolve_person(&db, "trent").await.unwrap() {
            Resolution::One(p) => assert_eq!(p.name, "Trent Rush"),
            other => panic!("expected One, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_duplicate_names_are_ambiguous() {
        let db = Db::open_memory().await.unwrap();
        seed(&db).await;
        add_person(
            &db,
            PersonInput {
                name: "Trent Adams".to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        match resolve_person(&db, "trent").await.unwrap() {
            Resolution::Ambiguous(people) => {
                let names: Vec<_> = people.iter().map(|p| p.name.as_str()).collect();
                assert!(names.contains(&"Trent Rush"));
                assert!(names.contains(&"Trent Adams"));
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }
}
