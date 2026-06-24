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

/// The voting rule, isolated from any DB.
///
/// `candidate` is the person we're inferring a boss for (never allowed to win,
/// guarding against self-reporting). `teammate_bosses` is each teammate's boss
/// id (one entry per teammate who has a `reports_to` edge).
///
/// Rule: the most-voted boss wins iff it has ≥2 votes AND ≥50% of all votes
/// cast. Ties at the top are rejected (no clear plurality).
pub fn tally_boss_vote(candidate: i64, teammate_bosses: &[i64]) -> Inference {
    // Tally votes, excluding any vote for the candidate themselves so we can
    // never infer someone as their own boss.
    let mut tally: HashMap<i64, usize> = HashMap::new();
    for &boss in teammate_bosses {
        if boss == candidate {
            continue;
        }
        *tally.entry(boss).or_insert(0) += 1;
    }

    let total: usize = tally.values().sum();
    if total == 0 {
        return Inference::NoGuess {
            reason: "no teammates have a reporting line".into(),
        };
    }

    // Find the top boss and detect a tie at the top.
    let mut best_boss = 0_i64;
    let mut best_votes = 0_usize;
    let mut tie = false;
    for (&boss, &votes) in &tally {
        if votes > best_votes {
            best_votes = votes;
            best_boss = boss;
            tie = false;
        } else if votes == best_votes {
            tie = true;
        }
    }

    if tie {
        return Inference::NoGuess {
            reason: format!("tie at {best_votes} vote(s) — no plurality"),
        };
    }
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
    let teammate_bosses = teammate_bosses(db, person, &team).await?;
    let verdict = tally_boss_vote(person, &teammate_bosses);

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

/// Bosses of every teammate (same team, not the person) who has a reporting line.
async fn teammate_bosses(db: &Db, person: i64, team: &str) -> Result<Vec<i64>> {
    let mut rows = db
        .conn()
        .query(
            "SELECT r.to_id
               FROM people p
               JOIN relationships r ON r.from_id = p.id AND r.kind = ?3
              WHERE p.team = ?1 AND p.id <> ?2",
            libsql::params![team, person, kind::REPORTS_TO],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<i64>(0)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edges::set_boss;
    use crate::people::{add_person, PersonInput};

    // --- pure rule tests (no DB) -------------------------------------------

    #[test]
    fn rule_needs_two_votes() {
        // One vote for boss 9 — not enough.
        assert!(matches!(
            tally_boss_vote(1, &[9]),
            Inference::NoGuess { .. }
        ));
    }

    #[test]
    fn rule_clear_majority_wins() {
        // Three teammates report to 9 -> 3/3, confidence 1.0.
        match tally_boss_vote(1, &[9, 9, 9]) {
            Inference::Inferred { boss_id, votes, total, confidence } => {
                assert_eq!((boss_id, votes, total), (9, 3, 3));
                assert!((confidence - 1.0).abs() < 1e-9);
            }
            other => panic!("expected inference, got {other:?}"),
        }
    }

    #[test]
    fn rule_plurality_threshold() {
        // 2 for boss 9, 2 for boss 8 -> tie, no guess.
        assert!(matches!(
            tally_boss_vote(1, &[9, 9, 8, 8]),
            Inference::NoGuess { .. }
        ));
        // 3 for 9, 1 for 8, 1 for 7 -> 3/5 = 60% >= 50%, wins.
        match tally_boss_vote(1, &[9, 9, 9, 8, 7]) {
            Inference::Inferred { boss_id, .. } => assert_eq!(boss_id, 9),
            other => panic!("expected inference, got {other:?}"),
        }
        // 2 for 9, 1 for 8, 1 for 7 -> 2/4 = exactly 50%, wins (>=).
        match tally_boss_vote(1, &[9, 9, 8, 7]) {
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
            tally_boss_vote(5, &[5, 5, 5]),
            Inference::NoGuess { .. }
        ));
        // Candidate's self-votes don't count toward the real boss either:
        // 2 real votes for 9, plus self-votes ignored -> 9 wins on 2/2.
        match tally_boss_vote(5, &[9, 9, 5]) {
            Inference::Inferred { boss_id, votes, total, .. } => {
                assert_eq!((boss_id, votes, total), (9, 2, 2));
            }
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
