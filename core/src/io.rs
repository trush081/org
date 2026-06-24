//! JSON export/import of the whole directory.
//!
//! Export is a full dump (people + relationships) for backup/transfer. Import
//! loads such a dump into a database, preserving ids so reporting lines stay
//! intact. Round-tripping export→import reproduces the graph exactly.

use crate::db::Db;
use crate::model::{Person, Relationship, Result};
use serde::{Deserialize, Serialize};

/// The serialized form of an entire directory. `version` lets a future format
/// change be detected on import without guessing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrgDump {
    pub version: u32,
    pub people: Vec<Person>,
    pub relationships: Vec<Relationship>,
}

/// Current dump format version.
const DUMP_VERSION: u32 = 1;

/// Read every person and relationship into an `OrgDump`.
pub async fn export(db: &Db) -> Result<OrgDump> {
    let people = all_people(db).await?;
    let relationships = all_relationships(db).await?;
    Ok(OrgDump {
        version: DUMP_VERSION,
        people,
        relationships,
    })
}

/// Export to a pretty-printed JSON string.
pub async fn export_json(db: &Db) -> Result<String> {
    let dump = export(db).await?;
    Ok(serde_json::to_string_pretty(&dump)?)
}

/// Load a dump into the database, preserving ids. Existing rows with the same
/// id are overwritten (INSERT OR REPLACE) so import is idempotent and ids in
/// reporting lines keep pointing at the right people.
pub async fn import(db: &Db, dump: &OrgDump) -> Result<()> {
    // `params!` consumes its arguments, and we're iterating by reference, so we
    // bind `&str` (which converts to a Value via borrow) for text columns and
    // copy the small scalar/Option fields. No full-row clone needed.
    for p in &dump.people {
        db.conn()
            .execute(
                "INSERT OR REPLACE INTO people
                   (id, name, team, title, notes, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                libsql::params![
                    p.id,
                    p.name.as_str(),
                    p.team.as_deref(),
                    p.title.as_deref(),
                    p.notes.as_deref(),
                    p.created_at.as_str(),
                    p.updated_at.as_str()
                ],
            )
            .await?;
    }
    for r in &dump.relationships {
        db.conn()
            .execute(
                "INSERT OR REPLACE INTO relationships
                   (id, from_id, to_id, kind, notes, confidence, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                libsql::params![
                    r.id,
                    r.from_id,
                    r.to_id,
                    r.kind.as_str(),
                    r.notes.as_deref(),
                    r.confidence,
                    r.source.as_str(),
                    r.created_at.as_str()
                ],
            )
            .await?;
    }
    Ok(())
}

/// Parse a JSON string and import it.
pub async fn import_json(db: &Db, json: &str) -> Result<()> {
    let dump: OrgDump = serde_json::from_str(json)?;
    import(db, &dump).await
}

async fn all_people(db: &Db) -> Result<Vec<Person>> {
    let mut rows = db
        .conn()
        .query(
            "SELECT id, name, team, title, notes, created_at, updated_at
               FROM people ORDER BY id",
            (),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Person {
            id: row.get(0)?,
            name: row.get(1)?,
            team: row.get(2)?,
            title: row.get(3)?,
            notes: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
        });
    }
    Ok(out)
}

async fn all_relationships(db: &Db) -> Result<Vec<Relationship>> {
    let mut rows = db
        .conn()
        .query(
            "SELECT id, from_id, to_id, kind, notes, confidence, source, created_at
               FROM relationships ORDER BY id",
            (),
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Relationship {
            id: row.get(0)?,
            from_id: row.get(1)?,
            to_id: row.get(2)?,
            kind: row.get(3)?,
            notes: row.get(4)?,
            confidence: row.get(5)?,
            source: row.get(6)?,
            created_at: row.get(7)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn export_then_import_roundtrips() {
        let src = Db::open_memory().await.unwrap();
        src.load_seed().await.unwrap();
        let json = export_json(&src).await.unwrap();

        // Load into a fresh DB and compare dumps.
        let dst = Db::open_memory().await.unwrap();
        import_json(&dst, &json).await.unwrap();

        let a = export(&src).await.unwrap();
        let b = export(&dst).await.unwrap();
        assert_eq!(a.people, b.people);
        assert_eq!(a.relationships, b.relationships);
    }

    #[tokio::test]
    async fn import_preserves_ids_and_reporting_lines() {
        let src = Db::open_memory().await.unwrap();
        src.load_seed().await.unwrap();
        let dump = export(&src).await.unwrap();

        let dst = Db::open_memory().await.unwrap();
        import(&dst, &dump).await.unwrap();

        // Trent (id 5) still reports to Pat (id 4) after import.
        let chain = crate::tree::chain_of_command(&dst, 5).await.unwrap();
        assert_eq!(chain[0].name, "Pat Smith");
    }

    #[tokio::test]
    async fn import_is_idempotent() {
        let src = Db::open_memory().await.unwrap();
        src.load_seed().await.unwrap();
        let dump = export(&src).await.unwrap();

        let dst = Db::open_memory().await.unwrap();
        import(&dst, &dump).await.unwrap();
        import(&dst, &dump).await.unwrap(); // second time: no duplicates

        let after = export(&dst).await.unwrap();
        assert_eq!(after.people.len(), dump.people.len());
        assert_eq!(after.relationships.len(), dump.relationships.len());
    }
}
