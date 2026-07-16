//! `org-core` — the shared brain: data model, persistence, and all logic.
//! No UI/CLI assumptions live here.

pub mod db;
pub mod edges;
pub mod infer;
pub mod io;
pub mod model;
pub mod people;
pub mod search;
pub mod seniority;
pub mod tree;

pub use db::{Config, Db};
pub use edges::set_boss;
pub use infer::{infer_boss, tally_boss_vote, BossVote, Inference};
pub use io::{export, export_json, import, import_json, OrgDump};
pub use model::{OrgError, Person, Relationship, Result};
pub use people::{add_person, get_person, list_people, remove_person, update_person, PersonInput};
pub use search::{fuzzy_search, resolve_person, Resolution, SearchHit};
pub use seniority::{rank_of, rank_title, Rank};
pub use tree::{
    chain_of_command, direct_reports, render_tree, roots, subtree, team_headcounts, Node,
};

#[cfg(test)]
mod cte_gate {
    //! Correctness gate: run the brief's reference CTEs against the seed data
    //! and assert the documented expected output. This runs before any logic is
    //! built on top, so a schema or query bug shows up at the cheapest moment.

    use crate::Db;

    /// Build a seeded in-memory DB for each test.
    async fn seeded() -> Db {
        let db = Db::open_memory().await.expect("open");
        db.load_seed().await.expect("seed");
        db
    }

    #[tokio::test]
    async fn chain_of_command_trent_is_just_pat() {
        let db = seeded().await;
        // Walk reports_to UPWARD from a person, cycle-guarded by depth < 100.
        let sql = r#"
            WITH RECURSIVE chain AS (
              SELECT p.id, p.name, 0 AS depth
              FROM people p WHERE p.id = ?1
              UNION ALL
              SELECT boss.id, boss.name, chain.depth + 1
              FROM chain
              JOIN relationships r ON r.from_id = chain.id AND r.kind = 'reports_to'
              JOIN people boss     ON boss.id = r.to_id
              WHERE chain.depth < 100
            )
            SELECT id, name, depth FROM chain WHERE depth > 0 ORDER BY depth;
        "#;
        let mut rows = db.conn().query(sql, [5_i64]).await.expect("query");

        let mut got = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            let name: String = row.get(1).expect("name");
            let depth: i64 = row.get(2).expect("depth");
            got.push((name, depth));
        }
        // Trent (5) -> Pat (4), and Pat is a root, so the chain is just Pat@1.
        assert_eq!(got, vec![("Pat Smith".to_string(), 1)]);
    }

    #[tokio::test]
    async fn direct_reports_of_pat() {
        let db = seeded().await;
        let sql = r#"
            SELECT p.id, p.name
            FROM relationships r
            JOIN people p ON p.id = r.from_id
            WHERE r.to_id = ?1 AND r.kind = 'reports_to'
            ORDER BY p.id;
        "#;
        let mut rows = db.conn().query(sql, [4_i64]).await.expect("query");

        let mut got = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            got.push(row.get::<String>(1).expect("name"));
        }
        // Pat's reports: Trent(5), Jane(6), Mike(7). NOT Trent->Jane mentors edge.
        assert_eq!(
            got,
            vec![
                "Trent Rush".to_string(),
                "Jane Doe".to_string(),
                "Mike Chen".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn roots_are_dana_and_pat() {
        let db = seeded().await;
        // Roots = people with no outgoing reports_to edge.
        let sql = r#"
            SELECT p.id, p.name
            FROM people p
            WHERE NOT EXISTS (
              SELECT 1 FROM relationships r
              WHERE r.from_id = p.id AND r.kind = 'reports_to'
            )
            ORDER BY p.id;
        "#;
        let mut rows = db.conn().query(sql, ()).await.expect("query");

        let mut got = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            got.push(row.get::<String>(1).expect("name"));
        }
        assert_eq!(got, vec!["Dana Cruz".to_string(), "Pat Smith".to_string()]);
    }

    #[tokio::test]
    async fn subtree_cte_walks_only_reports_to() {
        let db = seeded().await;
        // Full subtree DOWN from Pat (4): walk r.to_id = chain.id, take r.from_id.
        // A non-reporting edge must not create a second path into the subtree —
        // kinds are open TEXT, so simulate one.
        db.conn()
            .execute(
                "INSERT INTO relationships (from_id, to_id, kind, created_at)
                 VALUES (5, 6, 'peer_review', '2026-01-01')",
                (),
            )
            .await
            .expect("insert");
        let sql = r#"
            WITH RECURSIVE sub AS (
              SELECT p.id, p.name, 0 AS depth
              FROM people p WHERE p.id = ?1
              UNION ALL
              SELECT child.id, child.name, sub.depth + 1
              FROM sub
              JOIN relationships r ON r.to_id = sub.id AND r.kind = 'reports_to'
              JOIN people child    ON child.id = r.from_id
              WHERE sub.depth < 100
            )
            SELECT name, depth FROM sub WHERE depth > 0 ORDER BY name;
        "#;
        let mut rows = db.conn().query(sql, [4_i64]).await.expect("query");

        let mut got = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            let name: String = row.get(0).expect("name");
            let depth: i64 = row.get(1).expect("depth");
            got.push((name, depth));
        }
        // All three at depth 1; no duplicate Jane, no path through peer_review.
        assert_eq!(
            got,
            vec![
                ("Jane Doe".to_string(), 1),
                ("Mike Chen".to_string(), 1),
                ("Trent Rush".to_string(), 1),
            ]
        );
    }
}
