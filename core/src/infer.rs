//! Boss inference — the seed of the future AI hierarchy work.
//!
//! When someone joins a team with no `reports_to` edge, we guess their boss
//! from their teammates' reporting lines. The decision logic is a *pure*
//! function ([`tally_boss_vote`]) that takes the teammates' bosses and returns
//! a verdict — no DB, no async. That's deliberate: this is the swap point where
//! a real model replaces the heuristic later. [`infer_boss`] just gathers the
//! votes from SQL and applies the rule.

use crate::db::Db;
use crate::edges::RelateInput;
use crate::model::{kind, source, OrgError, Result};
use crate::seniority::{rank_of, Rank};
use std::collections::HashMap;

/// What the inference decided. Returned so callers (and the CLI) can tell the
/// user what was guessed and why, per the brief.
#[derive(Debug, Clone, PartialEq)]
pub enum Inference {
    /// A boss was inferred. `confidence` is the winner's vote share (0..1).
    Inferred {
        boss_id: i64,
        votes: usize,
        total: usize,
        confidence: f64,
    },
    /// Not enough signal: too few votes, no majority, or a tie.
    NoGuess { reason: String },
}

/// One teammate's vote: their boss's id, plus that boss's title seniority
/// (used only to break ties — see [`tally_boss_vote`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BossVote {
    pub boss_id: i64,
    pub boss_rank: Option<Rank>,
}

/// The voting rule, isolated from any DB.
///
/// `candidate` is the person we're inferring a boss for (never allowed to win,
/// guarding against self-reporting). `votes` holds each teammate's boss (one
/// entry per teammate who has a `reports_to` edge).
///
/// Rule: the most-voted boss wins iff it has ≥2 votes AND ≥50% of all votes
/// cast. A tie at the top falls back to title seniority: if exactly one of the
/// tied bosses outranks the rest, they win; otherwise no guess. Seniority never
/// overrides votes — it only settles what votes alone couldn't.
pub fn tally_boss_vote(candidate: i64, votes: &[BossVote]) -> Inference {
    // Tally votes, excluding any vote for the candidate themselves so we can
    // never infer someone as their own boss.
    let mut tally: HashMap<i64, usize> = HashMap::new();
    let mut ranks: HashMap<i64, Option<Rank>> = HashMap::new();
    for v in votes {
        if v.boss_id == candidate {
            continue;
        }
        *tally.entry(v.boss_id).or_insert(0) += 1;
        ranks.insert(v.boss_id, v.boss_rank);
    }

    let total: usize = tally.values().sum();
    if total == 0 {
        return Inference::NoGuess {
            reason: "no teammates have a reporting line".into(),
        };
    }

    // Find the top vote count and everyone who got it.
    let best_votes = *tally.values().max().expect("non-empty tally");
    let mut leaders: Vec<i64> = tally
        .iter()
        .filter(|&(_, &v)| v == best_votes)
        .map(|(&boss, _)| boss)
        .collect();

    let best_boss = if leaders.len() == 1 {
        leaders[0]
    } else {
        // Tie-break on seniority: the winner must *strictly* outrank every
        // other leader. Unknown ranks (None) can't win a tie-break — sorting
        // Option<Rank> puts None below any Some, which is the safe direction.
        leaders.sort_by_key(|boss| ranks[boss]);
        let top = *leaders.last().expect("at least two leaders");
        let runner_up = leaders[leaders.len() - 2];
        let outranks = match (ranks[&top], ranks[&runner_up]) {
            (Some(a), Some(b)) => a > b,
            (Some(_), None) => true,
            _ => false,
        };
        if !outranks {
            return Inference::NoGuess {
                reason: format!("tie at {best_votes} vote(s) — no plurality or seniority edge"),
            };
        }
        top
    };

    if best_votes < 2 {
        return Inference::NoGuess {
            reason: format!("only {best_votes} vote(s); need at least 2"),
        };
    }
    // ≥50% plurality. Integer math: best_votes * 2 >= total avoids float compare.
    if best_votes * 2 < total {
        return Inference::NoGuess {
            reason: format!("{best_votes}/{total} is below 50%"),
        };
    }

    Inference::Inferred {
        boss_id: best_boss,
        votes: best_votes,
        total,
        confidence: best_votes as f64 / total as f64,
    }
}

/// Infer and persist a boss for `person` from their team, if the rule fires.
///
/// Only acts when the person currently has NO `reports_to` edge — we never
/// overwrite an existing reporting line by inference. On success writes an edge
/// with source='inferred' and confidence = the vote share (< 1.0 unless every
/// teammate agreed), and returns the [`Inference`] so the caller can report it.
pub async fn infer_boss(db: &Db, person: i64) -> Result<Inference> {
    // Bail if this person already has a boss — don't override human/existing data.
    if current_boss(db, person).await?.is_some() {
        return Ok(Inference::NoGuess {
            reason: "person already has a reporting line".into(),
        });
    }

    let team = person_team(db, person).await?;
    let Some(team) = team else {
        return Ok(Inference::NoGuess {
            reason: "person has no team".into(),
        });
    };

    // Gather each teammate's boss (teammates = same team, excluding the person).
    let votes = teammate_boss_votes(db, person, &team).await?;
    let verdict = tally_boss_vote(person, &votes);

    if let Inference::Inferred {
        boss_id,
        confidence,
        ..
    } = &verdict
    {
        // Persist the inferred edge directly (bypassing set_boss, which is for
        // manual lines) so we control source/confidence.
        let mut edge = RelateInput::manual(person, *boss_id, kind::REPORTS_TO);
        edge.source = source::INFERRED.to_string();
        edge.confidence = *confidence;
        crate::edges::relate(db, edge).await?;
    }
    Ok(verdict)
}

/// The person's current boss id via reports_to, if any.
async fn current_boss(db: &Db, person: i64) -> Result<Option<i64>> {
    let mut rows = db
        .conn()
        .query(
            "SELECT to_id FROM relationships WHERE from_id = ?1 AND kind = ?2",
            libsql::params![person, kind::REPORTS_TO],
        )
        .await?;
    Ok(rows.next().await?.map(|r| r.get(0)).transpose()?)
}

/// The person's team, or None. Errors if the person doesn't exist.
async fn person_team(db: &Db, person: i64) -> Result<Option<String>> {
    let mut rows = db
        .conn()
        .query("SELECT team FROM people WHERE id = ?1", [person])
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Err(OrgError::PersonNotFound(person)),
    }
}

/// Bosses of every teammate (same team, not the person) who has a reporting
/// line, with each boss's title parsed to a rank for tie-breaking.
async fn teammate_boss_votes(db: &Db, person: i64, team: &str) -> Result<Vec<BossVote>> {
    let mut rows = db
        .conn()
        .query(
            "SELECT r.to_id, boss.title
               FROM people p
               JOIN relationships r ON r.from_id = p.id AND r.kind = ?3
               JOIN people boss     ON boss.id = r.to_id
              WHERE p.team = ?1 AND p.id <> ?2",
            libsql::params![team, person, kind::REPORTS_TO],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let title: Option<String> = row.get(1)?;
        out.push(BossVote {
            boss_id: row.get(0)?,
            boss_rank: rank_of(title.as_deref()),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edges::set_boss;
    use crate::people::{add_person, PersonInput};

    // --- pure rule tests (no DB) -------------------------------------------

    /// A vote with no seniority signal (unranked boss title).
    fn v(boss_id: i64) -> BossVote {
        BossVote { boss_id, boss_rank: None }
    }

    /// A vote for a boss whose title parsed to `rank`.
    fn vr(boss_id: i64, rank: Rank) -> BossVote {
        BossVote { boss_id, boss_rank: Some(rank) }
    }

    #[test]
    fn rule_needs_two_votes() {
        // One vote for boss 9 — not enough.
        assert!(matches!(
            tally_boss_vote(1, &[v(9)]),
            Inference::NoGuess { .. }
        ));
    }

    #[test]
    fn rule_clear_majority_wins() {
        // Three teammates report to 9 -> 3/3, confidence 1.0.
        match tally_boss_vote(1, &[v(9), v(9), v(9)]) {
            Inference::Inferred { boss_id, votes, total, confidence } => {
                assert_eq!((boss_id, votes, total), (9, 3, 3));
                assert!((confidence - 1.0).abs() < 1e-9);
            }
            other => panic!("expected inference, got {other:?}"),
        }
    }

    #[test]
    fn rule_plurality_threshold() {
        // 2 for boss 9, 2 for boss 8, no rank signal -> tie, no guess.
        assert!(matches!(
            tally_boss_vote(1, &[v(9), v(9), v(8), v(8)]),
            Inference::NoGuess { .. }
        ));
        // 3 for 9, 1 for 8, 1 for 7 -> 3/5 = 60% >= 50%, wins.
        match tally_boss_vote(1, &[v(9), v(9), v(9), v(8), v(7)]) {
            Inference::Inferred { boss_id, .. } => assert_eq!(boss_id, 9),
            other => panic!("expected inference, got {other:?}"),
        }
        // 2 for 9, 1 for 8, 1 for 7 -> 2/4 = exactly 50%, wins (>=).
        match tally_boss_vote(1, &[v(9), v(9), v(8), v(7)]) {
            Inference::Inferred { boss_id, votes, total, .. } => {
                assert_eq!((boss_id, votes, total), (9, 2, 4));
            }
            other => panic!("expected inference, got {other:?}"),
        }
    }

    #[test]
    fn rule_never_self() {
        // All "votes" are for the candidate themselves -> stripped -> no votes.
        assert!(matches!(
            tally_boss_vote(5, &[v(5), v(5), v(5)]),
            Inference::NoGuess { .. }
        ));
        // Candidate's self-votes don't count toward the real boss either:
        // 2 real votes for 9, plus self-votes ignored -> 9 wins on 2/2.
        match tally_boss_vote(5, &[v(9), v(9), v(5)]) {
            Inference::Inferred { boss_id, votes, total, .. } => {
                assert_eq!((boss_id, votes, total), (9, 2, 2));
            }
            other => panic!("expected inference, got {other:?}"),
        }
    }

    #[test]
    fn rule_tie_breaks_on_seniority() {
        // 2-2 tie, but boss 9 is a Director and boss 8 a Manager -> 9 wins.
        match tally_boss_vote(
            1,
            &[vr(9, Rank::Director), vr(9, Rank::Director), vr(8, Rank::Manager), vr(8, Rank::Manager)],
        ) {
            Inference::Inferred { boss_id, votes, total, confidence } => {
                assert_eq!((boss_id, votes, total), (9, 2, 4));
                assert!((confidence - 0.5).abs() < 1e-9); // still just the vote share
            }
            other => panic!("expected inference, got {other:?}"),
        }
        // A ranked boss beats an unranked one.
        match tally_boss_vote(1, &[vr(9, Rank::Manager), vr(9, Rank::Manager), v(8), v(8)]) {
            Inference::Inferred { boss_id, .. } => assert_eq!(boss_id, 9),
            other => panic!("expected inference, got {other:?}"),
        }
    }

    #[test]
    fn rule_tie_with_equal_or_unknown_rank_stays_no_guess() {
        // Same rank on both sides: seniority can't settle it.
        assert!(matches!(
            tally_boss_vote(1, &[vr(9, Rank::Manager), vr(9, Rank::Manager), vr(8, Rank::Manager), vr(8, Rank::Manager)]),
            Inference::NoGuess { .. }
        ));
        // Both unranked: likewise.
        assert!(matches!(
            tally_boss_vote(1, &[v(9), v(9), v(8), v(8)]),
            Inference::NoGuess { .. }
        ));
        // Seniority must never override votes: 8 has MORE votes than the
        // Director; the Director's rank is irrelevant.
        match tally_boss_vote(1, &[v(8), v(8), v(8), vr(9, Rank::Director)]) {
            Inference::Inferred { boss_id, .. } => assert_eq!(boss_id, 8),
            other => panic!("expected inference, got {other:?}"),
        }
    }

    // --- integration test against seed-shaped data -------------------------

    async fn person(db: &Db, name: &str, team: &str) -> i64 {
        add_person(
            db,
            PersonInput {
                name: name.to_string(),
                team: Some(team.to_string()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    #[tokio::test]
    async fn infers_boss_from_teammates_and_persists_inferred_edge() {
        let db = Db::open_memory().await.unwrap();
        let pat = person(&db, "Pat", "IDS").await;
        let trent = person(&db, "Trent", "IDS").await;
        let jane = person(&db, "Jane", "IDS").await;
        set_boss(&db, trent, pat).await.unwrap();
        set_boss(&db, jane, pat).await.unwrap();

        // New hire on IDS, no boss yet.
        let mike = person(&db, "Mike", "IDS").await;
        let verdict = infer_boss(&db, mike).await.unwrap();

        match verdict {
            Inference::Inferred { boss_id, confidence, .. } => {
                assert_eq!(boss_id, pat);
                assert!((confidence - 1.0).abs() < 1e-9); // both teammates agree
            }
            other => panic!("expected inference, got {other:?}"),
        }

        // The persisted edge is marked inferred, not manual.
        let mut rows = db
            .conn()
            .query(
                "SELECT to_id, source, confidence FROM relationships
                  WHERE from_id = ?1 AND kind = 'reports_to'",
                [mike],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), pat);
        assert_eq!(row.get::<String>(1).unwrap(), "inferred");
    }

    #[tokio::test]
    async fn does_not_override_existing_boss() {
        let db = Db::open_memory().await.unwrap();
        let pat = person(&db, "Pat", "IDS").await;
        let other = person(&db, "Other", "IDS").await;
        let trent = person(&db, "Trent", "IDS").await;
        let jane = person(&db, "Jane", "IDS").await;
        set_boss(&db, jane, pat).await.unwrap();
        set_boss(&db, other, pat).await.unwrap();
        // Trent already reports to someone else (a one-off line).
        set_boss(&db, trent, other).await.unwrap();

        let verdict = infer_boss(&db, trent).await.unwrap();
        assert!(matches!(verdict, Inference::NoGuess { .. }));
    }
}
