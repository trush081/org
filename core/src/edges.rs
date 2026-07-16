//! Relationship (edge) writes. All SQL for the `relationships` table lives here.
//!
//! The hierarchy is just `reports_to` edges, so there's no special boss column —
//! `set_boss` is `relate` with a fixed kind plus the delete-old-first rule.

use crate::db::Db;
use crate::model::{kind, source, OrgError, Result};

/// Current UTC timestamp as ISO8601 (RFC3339).
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Inputs for creating/replacing an edge. Crate-internal: the public surface
/// for reporting lines is `set_boss`; generic edges are machinery for
/// inference (and future AI features), not a user-facing feature.
#[derive(Debug, Clone)]
pub(crate) struct RelateInput {
    pub from_id: i64,
    pub to_id: i64,
    pub kind: String,
    pub notes: Option<String>,
    /// 0..1. Human-entered edges are 1.0; AI-inferred are < 1.0.
    pub confidence: f64,
    /// 'manual' | 'inferred' | 'imported'.
    pub source: String,
}

impl RelateInput {
    /// A manual edge of the given kind: confidence 1.0, source 'manual'.
    pub(crate) fn manual(from_id: i64, to_id: i64, kind: impl Into<String>) -> Self {
        RelateInput {
            from_id,
            to_id,
            kind: kind.into(),
            notes: None,
            confidence: 1.0,
            source: source::MANUAL.to_string(),
        }
    }
}

/// Set or replace an edge of a given kind between two people.
///
/// "Replace" is enforced by `INSERT ... ON CONFLICT(from_id,to_id,kind) DO
/// UPDATE`: the UNIQUE(from_id,to_id,kind) constraint means a second relate of
/// the same triple updates the existing row instead of erroring or duplicating.
/// This is the right semantics for a *specific* directed pairing of one kind.
///
/// Note: this does NOT delete other edges of the same kind from `from_id` to a
/// *different* target. For reporting lines that's wrong (a person has one boss),
/// which is exactly why `set_boss` exists separately.
pub(crate) async fn relate(db: &Db, input: RelateInput) -> Result<()> {
    validate_pair(&input)?;
    let ts = now();
    db.conn()
        .execute(
            "INSERT INTO relationships (from_id, to_id, kind, notes, confidence, source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(from_id, to_id, kind) DO UPDATE SET
               notes = excluded.notes,
               confidence = excluded.confidence,
               source = excluded.source",
            libsql::params![
                input.from_id,
                input.to_id,
                input.kind,
                input.notes,
                input.confidence,
                input.source,
                ts
            ],
        )
        .await?;
    Ok(())
}

/// Set `person`'s boss to `boss`, replacing any existing reporting line.
///
/// Hard requirement: changing a boss DELETEs the old `reports_to` edge(s) from
/// `person`, then INSERTs the new one — so stale reporting lines never
/// accumulate. We delete by `from_id` + kind (not the full triple) precisely to
/// catch a *previously different* boss, which ON CONFLICT alone would miss.
pub async fn set_boss(db: &Db, person: i64, boss: i64) -> Result<()> {
    if person == boss {
        return Err(OrgError::Invalid(format!(
            "person {person} cannot report to themselves"
        )));
    }
    ensure_exists(db, person).await?;
    ensure_exists(db, boss).await?;

    let ts = now();
    // Two statements, same connection. SQLite gives each statement its own
    // implicit transaction; for v1 that's acceptable. If a crash between them
    // ever matters we'd wrap in BEGIN/COMMIT — noted, not needed yet.
    db.conn()
        .execute(
            "DELETE FROM relationships WHERE from_id = ?1 AND kind = ?2",
            libsql::params![person, kind::REPORTS_TO],
        )
        .await?;
    db.conn()
        .execute(
            "INSERT INTO relationships (from_id, to_id, kind, confidence, source, created_at)
             VALUES (?1, ?2, ?3, 1.0, ?4, ?5)",
            libsql::params![person, boss, kind::REPORTS_TO, source::MANUAL, ts],
        )
        .await?;
    Ok(())
}

/// Reject self-edges and (cheaply) validate confidence range.
fn validate_pair(input: &RelateInput) -> Result<()> {
    if input.from_id == input.to_id {
        return Err(OrgError::Invalid(format!(
            "self-relationship not allowed (id {})",
            input.from_id
        )));
    }
    if !(0.0..=1.0).contains(&input.confidence) {
        return Err(OrgError::Invalid(format!(
            "confidence {} out of range 0..1",
            input.confidence
        )));
    }
    Ok(())
}

/// Error if a person id isn't present. Foreign keys would catch a bad insert,
/// but this gives a clean PersonNotFound instead of a raw FK error.
async fn ensure_exists(db: &Db, id: i64) -> Result<()> {
    let mut rows = db
        .conn()
        .query("SELECT 1 FROM people WHERE id = ?1", [id])
        .await?;
    if rows.next().await?.is_none() {
        return Err(OrgError::PersonNotFound(id));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::people::{add_person, PersonInput};

    async fn person(db: &Db, name: &str) -> i64 {
        add_person(
            db,
            PersonInput {
                name: name.to_string(),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id
    }

    /// Read the current boss id for a person, if any.
    async fn boss_of(db: &Db, person: i64) -> Option<i64> {
        let mut rows = db
            .conn()
            .query(
                "SELECT to_id FROM relationships WHERE from_id = ?1 AND kind = 'reports_to'",
                [person],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().map(|r| r.get(0).unwrap())
    }

    #[tokio::test]
    async fn set_boss_replaces_not_accumulates() {
        let db = Db::open_memory().await.unwrap();
        let emp = person(&db, "Emp").await;
        let boss1 = person(&db, "Boss1").await;
        let boss2 = person(&db, "Boss2").await;

        set_boss(&db, emp, boss1).await.unwrap();
        assert_eq!(boss_of(&db, emp).await, Some(boss1));

        set_boss(&db, emp, boss2).await.unwrap();
        // New boss is set...
        assert_eq!(boss_of(&db, emp).await, Some(boss2));
        // ...and there is exactly ONE reports_to edge, not two.
        let mut rows = db
            .conn()
            .query(
                "SELECT COUNT(*) FROM relationships WHERE from_id = ?1 AND kind = 'reports_to'",
                [emp],
            )
            .await
            .unwrap();
        let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn set_boss_rejects_self() {
        let db = Db::open_memory().await.unwrap();
        let p = person(&db, "Solo").await;
        assert!(matches!(
            set_boss(&db, p, p).await.unwrap_err(),
            OrgError::Invalid(_)
        ));
    }

    #[tokio::test]
    async fn set_boss_unknown_person_errors() {
        let db = Db::open_memory().await.unwrap();
        let p = person(&db, "Real").await;
        assert!(matches!(
            set_boss(&db, p, 999).await.unwrap_err(),
            OrgError::PersonNotFound(999)
        ));
    }

    #[tokio::test]
    async fn relate_is_idempotent_replace() {
        let db = Db::open_memory().await.unwrap();
        let a = person(&db, "A").await;
        let b = person(&db, "B").await;

        // Kinds are open TEXT; any non-reporting kind exercises the same path.
        relate(&db, RelateInput::manual(a, b, "peer_review")).await.unwrap();
        // Relate the same triple again with new notes -> updates, no duplicate.
        let mut input = RelateInput::manual(a, b, "peer_review");
        input.notes = Some("pairing".into());
        relate(&db, input).await.unwrap();

        let mut rows = db
            .conn()
            .query(
                "SELECT COUNT(*), MAX(notes) FROM relationships WHERE from_id=?1 AND to_id=?2 AND kind=?3",
                libsql::params![a, b, "peer_review"],
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 1);
        assert_eq!(row.get::<String>(1).unwrap(), "pairing");
    }

    #[tokio::test]
    async fn relate_rejects_self_and_bad_confidence() {
        let db = Db::open_memory().await.unwrap();
        let a = person(&db, "A").await;
        assert!(relate(&db, RelateInput::manual(a, a, "peer_review")).await.is_err());

        let b = person(&db, "B").await;
        let mut bad = RelateInput::manual(a, b, "peer_review");
        bad.confidence = 1.5;
        assert!(relate(&db, bad).await.is_err());
    }

    #[tokio::test]
    async fn cascade_delete_removes_edges() {
        let db = Db::open_memory().await.unwrap();
        let a = person(&db, "A").await;
        let b = person(&db, "B").await;
        set_boss(&db, a, b).await.unwrap();

        // Deleting the boss should cascade-remove the reports_to edge.
        crate::people::remove_person(&db, b).await.unwrap();
        assert_eq!(boss_of(&db, a).await, None);
    }
}
