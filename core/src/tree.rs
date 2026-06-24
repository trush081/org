//! Hierarchy traversal via recursive CTEs. All reporting-line reads live here.
//!
//! Every recursive query carries a `depth < 100` cycle guard. Nothing in the
//! schema prevents an A→B→A `reports_to` loop (UNIQUE only blocks identical
//! duplicate edges), so the depth cap is the v1 defense against infinite
//! recursion. Never add a recursive CTE here without it.

use crate::db::Db;
use crate::model::Result;

/// A node in a traversal result: a person plus their depth from the anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: i64,
    pub name: String,
    /// Distance from the query's anchor person (anchor itself = 0).
    pub depth: i64,
}

/// Chain of command: walk `reports_to` UPWARD from `person`, nearest boss first.
/// Excludes the person themselves (depth 0). Empty if they're a root.
pub async fn chain_of_command(db: &Db, person: i64) -> Result<Vec<Node>> {
    // Recursive step joins each row to its boss (r.from_id = chain.id, take to_id).
    let sql = r#"
        WITH RECURSIVE chain AS (
          SELECT p.id, p.name, 0 AS depth
          FROM people p WHERE p.id = ?1
          UNION ALL
          SELECT boss.id, boss.name, chain.depth + 1
          FROM chain
          JOIN relationships r ON r.from_id = chain.id AND r.kind = 'reports_to'
          JOIN people boss     ON boss.id = r.to_id
          WHERE chain.depth < 100          -- cycle guard
        )
        SELECT id, name, depth FROM chain WHERE depth > 0 ORDER BY depth;
    "#;
    collect_nodes(db, sql, person).await
}

/// Direct reports: one level down, no recursion. Ordered by name.
pub async fn direct_reports(db: &Db, person: i64) -> Result<Vec<Node>> {
    let sql = r#"
        SELECT p.id, p.name, 1 AS depth
        FROM relationships r
        JOIN people p ON p.id = r.from_id
        WHERE r.to_id = ?1 AND r.kind = 'reports_to'
        ORDER BY p.name;
    "#;
    collect_nodes(db, sql, person).await
}

/// Full subtree: everyone under `person`, any depth. Chain inverted — recursive
/// step walks DOWN (r.to_id = sub.id, take r.from_id). Excludes the anchor.
/// Ordered by depth then name for a stable, tree-like sequence.
pub async fn subtree(db: &Db, person: i64) -> Result<Vec<Node>> {
    let sql = r#"
        WITH RECURSIVE sub AS (
          SELECT p.id, p.name, 0 AS depth
          FROM people p WHERE p.id = ?1
          UNION ALL
          SELECT child.id, child.name, sub.depth + 1
          FROM sub
          JOIN relationships r ON r.to_id = sub.id AND r.kind = 'reports_to'
          JOIN people child    ON child.id = r.from_id
          WHERE sub.depth < 100            -- cycle guard
        )
        SELECT id, name, depth FROM sub WHERE depth > 0 ORDER BY depth, name;
    "#;
    collect_nodes(db, sql, person).await
}

/// Roots: people with no outgoing `reports_to` edge (nobody they report to).
/// Returned as depth-0 nodes, name order.
pub async fn roots(db: &Db) -> Result<Vec<Node>> {
    let sql = r#"
        SELECT p.id, p.name, 0 AS depth
        FROM people p
        WHERE NOT EXISTS (
          SELECT 1 FROM relationships r
          WHERE r.from_id = p.id AND r.kind = 'reports_to'
        )
        ORDER BY p.name;
    "#;
    let mut rows = db.conn().query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Node {
            id: row.get(0)?,
            name: row.get(1)?,
            depth: row.get(2)?,
        });
    }
    Ok(out)
}

/// Per-team headcounts, descending by count then team name.
/// People with NULL team are grouped under "(no team)".
pub async fn team_headcounts(db: &Db) -> Result<Vec<(String, i64)>> {
    let sql = r#"
        SELECT COALESCE(team, '(no team)') AS t, COUNT(*) AS n
        FROM people
        GROUP BY t
        ORDER BY n DESC, t ASC;
    "#;
    let mut rows = db.conn().query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push((row.get::<String>(0)?, row.get::<i64>(1)?));
    }
    Ok(out)
}

/// Render the subtree under `person` (inclusive) as an indented text tree.
/// Two-space indent per level. Built from `subtree` plus the anchor's own name.
pub async fn render_tree(db: &Db, person: i64) -> Result<String> {
    // Anchor line first.
    let anchor = crate::people::get_person(db, person)
        .await?
        .ok_or(crate::model::OrgError::PersonNotFound(person))?;

    let mut out = String::new();
    out.push_str(&anchor.name);
    out.push('\n');

    for node in subtree(db, person).await? {
        // depth 1 -> two spaces, depth 2 -> four, etc.
        for _ in 0..node.depth {
            out.push_str("  ");
        }
        out.push_str(&node.name);
        out.push('\n');
    }
    Ok(out)
}

/// Shared helper: run a `(id, name, depth)` query with one id param.
async fn collect_nodes(db: &Db, sql: &str, person: i64) -> Result<Vec<Node>> {
    let mut rows = db.conn().query(sql, [person]).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Node {
            id: row.get(0)?,
            name: row.get(1)?,
            depth: row.get(2)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Seeded DB matching the brief's fixture (ids 1..7).
    async fn seeded() -> Db {
        let db = Db::open_memory().await.unwrap();
        db.load_seed().await.unwrap();
        db
    }

    #[tokio::test]
    async fn chain_of_trent_is_just_pat() {
        let db = seeded().await;
        let chain = chain_of_command(&db, 5).await.unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].name, "Pat Smith");
        assert_eq!(chain[0].depth, 1);
    }

    #[tokio::test]
    async fn direct_reports_of_pat() {
        let db = seeded().await;
        let reports = direct_reports(&db, 4).await.unwrap();
        let names: Vec<_> = reports.iter().map(|n| n.name.as_str()).collect();
        // Name-ordered: Jane, Mike, Trent. The mentors edge does not appear.
        assert_eq!(names, vec!["Jane Doe", "Mike Chen", "Trent Rush"]);
    }

    #[tokio::test]
    async fn subtree_of_pat_excludes_mentors_path() {
        let db = seeded().await;
        let sub = subtree(&db, 4).await.unwrap();
        let names: Vec<_> = sub.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Jane Doe", "Mike Chen", "Trent Rush"]);
        assert!(sub.iter().all(|n| n.depth == 1));
    }

    #[tokio::test]
    async fn roots_are_dana_and_pat() {
        let db = seeded().await;
        let r = roots(&db).await.unwrap();
        let names: Vec<_> = r.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Dana Cruz", "Pat Smith"]);
    }

    #[tokio::test]
    async fn headcounts_by_team() {
        let db = seeded().await;
        let counts = team_headcounts(&db).await.unwrap();
        // IDS Fulfillment has 4, Marketing has 3; IDS first (count desc).
        assert_eq!(counts[0], ("IDS Fulfillment".to_string(), 4));
        assert_eq!(counts[1], ("Marketing Delivery Tracking".to_string(), 3));
    }

    #[tokio::test]
    async fn render_tree_indents() {
        let db = seeded().await;
        let text = render_tree(&db, 4).await.unwrap();
        // Pat at root, three reports indented two spaces.
        assert!(text.starts_with("Pat Smith\n"));
        assert!(text.contains("\n  Trent Rush\n"));
        assert!(text.contains("\n  Jane Doe\n"));
    }

    #[tokio::test]
    async fn cycle_guard_terminates() {
        // Build A->B->A reports_to loop and confirm traversal stops, no hang.
        let db = Db::open_memory().await.unwrap();
        use crate::people::{add_person, PersonInput};
        let a = add_person(&db, PersonInput { name: "A".into(), ..Default::default() })
            .await.unwrap().id;
        let b = add_person(&db, PersonInput { name: "B".into(), ..Default::default() })
            .await.unwrap().id;
        // Insert raw edges to bypass set_boss's self/replace logic.
        crate::edges::relate(&db, crate::edges::RelateInput::manual(a, b, "reports_to"))
            .await.unwrap();
        crate::edges::relate(&db, crate::edges::RelateInput::manual(b, a, "reports_to"))
            .await.unwrap();

        // Must return within the depth cap rather than loop forever.
        let chain = chain_of_command(&db, a).await.unwrap();
        assert!(chain.len() <= 100);
        assert!(chain.len() >= 2); // it does walk the loop, just bounded
    }
}
