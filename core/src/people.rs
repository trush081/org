//! Person CRUD. All SQL for the `people` table lives here.

use crate::db::Db;
use crate::model::{OrgError, Person, Result};
use libsql::Row;

/// Current UTC timestamp as ISO8601 (RFC3339). One place so every write agrees.
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Map a `people` row (selected in column order below) into a `Person`.
/// Centralized so the column list and the struct never drift apart.
fn row_to_person(row: &Row) -> Result<Person> {
    Ok(Person {
        id: row.get(0)?,
        name: row.get(1)?,
        team: row.get(2)?,   // Option<String> — libsql maps SQL NULL to None
        title: row.get(3)?,
        notes: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

/// The SELECT column list, shared by every read so order matches row_to_person.
const PERSON_COLS: &str = "id, name, team, title, notes, created_at, updated_at";

/// Fields for creating/updating a person. `None` means "leave unset/NULL".
#[derive(Debug, Default, Clone)]
pub struct PersonInput {
    pub name: String,
    pub team: Option<String>,
    pub title: Option<String>,
    pub notes: Option<String>,
}

/// Insert a new person; returns the row with its assigned id and timestamps.
pub async fn add_person(db: &Db, input: PersonInput) -> Result<Person> {
    let ts = now();
    db.conn()
        .execute(
            "INSERT INTO people (name, team, title, notes, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            // params! lets us mix String/Option<String> in one positional list;
            // a plain array would force a single homogeneous element type.
            libsql::params![input.name, input.team, input.title, input.notes, ts],
        )
        .await?;

    let id = db.conn().last_insert_rowid();
    // Re-read so the caller gets exactly what's stored (timestamps included).
    get_person(db, id)
        .await?
        .ok_or(OrgError::PersonNotFound(id))
}

/// Fetch one person by id, or `None` if absent.
pub async fn get_person(db: &Db, id: i64) -> Result<Option<Person>> {
    let sql = format!("SELECT {PERSON_COLS} FROM people WHERE id = ?1");
    let mut rows = db.conn().query(&sql, [id]).await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row_to_person(&row)?)),
        None => Ok(None),
    }
}

/// Replace a person's mutable fields wholesale and bump updated_at.
/// `name` is required; team/title/notes follow the input (None clears them).
pub async fn update_person(db: &Db, id: i64, input: PersonInput) -> Result<Person> {
    let ts = now();
    let affected = db
        .conn()
        .execute(
            "UPDATE people
                SET name = ?2, team = ?3, title = ?4, notes = ?5, updated_at = ?6
              WHERE id = ?1",
            libsql::params![id, input.name, input.team, input.title, input.notes, ts],
        )
        .await?;

    if affected == 0 {
        return Err(OrgError::PersonNotFound(id));
    }
    get_person(db, id)
        .await?
        .ok_or(OrgError::PersonNotFound(id))
}

/// Delete a person. ON DELETE CASCADE removes their relationship edges too
/// (which is why `PRAGMA foreign_keys = ON` matters). Errors if no such person.
pub async fn remove_person(db: &Db, id: i64) -> Result<()> {
    let affected = db
        .conn()
        .execute("DELETE FROM people WHERE id = ?1", [id])
        .await?;
    if affected == 0 {
        return Err(OrgError::PersonNotFound(id));
    }
    Ok(())
}

/// All people, most senior title first (unknown titles last), then by name —
/// the same sibling order the tree uses. Used by `list`/`export`/search.
pub async fn list_people(db: &Db, team: Option<&str>) -> Result<Vec<Person>> {
    let mut out = Vec::new();
    let mut rows = match team {
        Some(t) => {
            let sql = format!("SELECT {PERSON_COLS} FROM people WHERE team = ?1");
            db.conn().query(&sql, [t]).await?
        }
        None => {
            let sql = format!("SELECT {PERSON_COLS} FROM people");
            db.conn().query(&sql, ()).await?
        }
    };
    while let Some(row) = rows.next().await? {
        out.push(row_to_person(&row)?);
    }
    // Rank in Rust, not SQL — titles are free text (see crate::seniority).
    out.sort_by_key(|p| {
        (
            std::cmp::Reverse(crate::seniority::rank_of(p.title.as_deref())),
            p.name.clone(),
        )
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(name: &str) -> PersonInput {
        PersonInput {
            name: name.to_string(),
            team: Some("IDS Fulfillment".to_string()),
            title: Some("SWE".to_string()),
            notes: None,
        }
    }

    #[tokio::test]
    async fn add_then_get_roundtrips() {
        let db = Db::open_memory().await.unwrap();
        let p = add_person(&db, input("Sam Park")).await.unwrap();
        assert!(p.id > 0);
        assert_eq!(p.name, "Sam Park");
        assert_eq!(p.team.as_deref(), Some("IDS Fulfillment"));
        assert_eq!(p.notes, None);
        // created_at == updated_at on insert (same bound param ?5).
        assert_eq!(p.created_at, p.updated_at);

        let fetched = get_person(&db, p.id).await.unwrap().unwrap();
        assert_eq!(fetched, p);
    }

    #[tokio::test]
    async fn get_missing_is_none() {
        let db = Db::open_memory().await.unwrap();
        assert!(get_person(&db, 999).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn update_changes_fields_and_bumps_timestamp() {
        let db = Db::open_memory().await.unwrap();
        let p = add_person(&db, input("Sam Park")).await.unwrap();

        let mut upd = input("Samuel Park");
        upd.title = Some("Sr SWE".to_string());
        upd.team = None; // clearing a field
        let after = update_person(&db, p.id, upd).await.unwrap();

        assert_eq!(after.name, "Samuel Park");
        assert_eq!(after.title.as_deref(), Some("Sr SWE"));
        assert_eq!(after.team, None);
        assert_eq!(after.created_at, p.created_at); // unchanged
        assert!(after.updated_at >= p.updated_at); // bumped (RFC3339 sorts lexically)
    }

    #[tokio::test]
    async fn update_missing_errors() {
        let db = Db::open_memory().await.unwrap();
        let err = update_person(&db, 42, input("Ghost")).await.unwrap_err();
        assert!(matches!(err, OrgError::PersonNotFound(42)));
    }

    #[tokio::test]
    async fn remove_missing_errors_present_succeeds() {
        let db = Db::open_memory().await.unwrap();
        assert!(matches!(
            remove_person(&db, 7).await.unwrap_err(),
            OrgError::PersonNotFound(7)
        ));
        let p = add_person(&db, input("Sam Park")).await.unwrap();
        remove_person(&db, p.id).await.unwrap();
        assert!(get_person(&db, p.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_filters_by_team() {
        let db = Db::open_memory().await.unwrap();
        add_person(&db, input("Bea")).await.unwrap();
        let mut other = input("Cy");
        other.team = Some("Marketing".to_string());
        add_person(&db, other).await.unwrap();

        let ids = list_people(&db, Some("IDS Fulfillment")).await.unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].name, "Bea");
        assert_eq!(list_people(&db, None).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_orders_by_seniority_then_name() {
        let db = Db::open_memory().await.unwrap();
        for (name, title) in [
            ("Zoe", Some("Director")),
            ("Al", Some("SWE II")),
            ("Bo", Some("Sr Engineer")),
            ("Cy", None), // no title -> last
            ("Ann", Some("SWE II")),
        ] {
            add_person(
                &db,
                PersonInput {
                    name: name.to_string(),
                    title: title.map(String::from),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let names: Vec<_> = list_people(&db, None)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        // Director > Senior > the two SWE IIs (name order) > untitled.
        assert_eq!(names, vec!["Zoe", "Bo", "Al", "Ann", "Cy"]);
    }
}
