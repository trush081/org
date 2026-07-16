//! Hierarchy traversal via recursive CTEs. All reporting-line reads live here.
//!
//! Every recursive query carries a `depth < 100` cycle guard. Nothing in the
//! schema prevents an A→B→A `reports_to` loop (UNIQUE only blocks identical
//! duplicate edges), so the depth cap is the v1 defense against infinite
//! recursion. Never add a recursive CTE here without it.
//!
//! Siblings (people under the same boss) are ordered by title seniority,
//! most senior first — a Sr Engineer lists above an SWE II. Ranking happens in
//! Rust (see [`crate::seniority`]) because titles are free text the SQL layer
//! can't meaningfully compare.

use crate::db::Db;
use crate::model::Result;
use crate::seniority::{rank_of, Rank};
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

/// A node in a traversal result: a person plus their depth from the anchor.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub id: i64,
    pub name: String,
    pub title: Option<String>,
    /// Distance from the query's anchor person (anchor itself = 0).
    pub depth: i64,
}

impl Node {
    /// Sort key for siblings: seniority descending, unknowns last, then name.
    ///
    /// `Reverse` flips the comparison so higher ranks sort first. It also flips
    /// `Option`'s ordering (`None < Some(_)`), which is exactly what puts
    /// unrecognized titles at the *end* instead of the front.
    fn sibling_key(&self) -> (Reverse<Option<Rank>>, String) {
        (Reverse(rank_of(self.title.as_deref())), self.name.clone())
    }
}

/// Chain of command: walk `reports_to` UPWARD from `person`, nearest boss first.
/// Excludes the person themselves (depth 0). Empty if they're a root.
pub async fn chain_of_command(db: &Db, person: i64) -> Result<Vec<Node>> {
    // Recursive step joins each row to its boss (r.from_id = chain.id, take to_id).
    let sql = r#"
        WITH RECURSIVE chain AS (
          SELECT p.id, p.name, p.title, 0 AS depth
          FROM people p WHERE p.id = ?1
          UNION ALL
          SELECT boss.id, boss.name, boss.title, chain.depth + 1
          FROM chain
          JOIN relationships r ON r.from_id = chain.id AND r.kind = 'reports_to'
          JOIN people boss     ON boss.id = r.to_id
          WHERE chain.depth < 100          -- cycle guard
        )
        SELECT id, name, title, depth FROM chain WHERE depth > 0 ORDER BY depth;
    "#;
    collect_nodes(db, sql, person).await
}

/// Direct reports: one level down, no recursion. Most senior first.
pub async fn direct_reports(db: &Db, person: i64) -> Result<Vec<Node>> {
    let sql = r#"
        SELECT p.id, p.name, p.title, 1 AS depth
        FROM relationships r
        JOIN people p ON p.id = r.from_id
        WHERE r.to_id = ?1 AND r.kind = 'reports_to';
    "#;
    let mut nodes = collect_nodes(db, sql, person).await?;
    nodes.sort_by_key(Node::sibling_key);
    Ok(nodes)
}

/// Full subtree: everyone under `person`, any depth. Excludes the anchor.
///
/// Returned in *pre-order*: each person is immediately followed by their own
/// reports, and siblings run most-senior-first. Indenting each node by its
/// depth therefore reads as a correct tree — children always sit directly
/// under their parent, never after an unrelated branch.
pub async fn subtree(db: &Db, person: i64) -> Result<Vec<Node>> {
    // The CTE gathers the raw (parent, child) rows; ordering is done in Rust
    // because sibling order depends on title parsing.
    let sql = r#"
        WITH RECURSIVE sub AS (
          SELECT p.id, p.name, p.title, 0 AS depth
          FROM people p WHERE p.id = ?1
          UNION ALL
          SELECT child.id, child.name, child.title, sub.depth + 1
          FROM sub
          JOIN relationships r ON r.to_id = sub.id AND r.kind = 'reports_to'
          JOIN people child    ON child.id = r.from_id
          WHERE sub.depth < 100            -- cycle guard
        )
        SELECT s.id, s.name, s.title, s.depth, r.to_id AS parent
        FROM sub s
        JOIN relationships r ON r.from_id = s.id AND r.kind = 'reports_to'
        WHERE s.depth > 0;
    "#;
    let mut rows = db.conn().query(sql, [person]).await?;

    // children[boss_id] = that boss's direct reports.
    let mut children: HashMap<i64, Vec<Node>> = HashMap::new();
    while let Some(row) = rows.next().await? {
        let node = Node {
            id: row.get(0)?,
            name: row.get(1)?,
            title: row.get(2)?,
            depth: row.get(3)?,
        };
        let parent: i64 = row.get(4)?;
        children.entry(parent).or_default().push(node);
    }
    for siblings in children.values_mut() {
        siblings.sort_by_key(Node::sibling_key);
    }

    // Flatten to pre-order with an explicit stack (iterative DFS). `visited`
    // is the in-Rust cycle guard: the CTE's depth cap bounds the *rows*, but a
    // reports_to loop in the children map would still recurse forever here.
    let mut out = Vec::new();
    let mut visited: HashSet<i64> = HashSet::from([person]);
    // Stack of (node, depth); push siblings reversed so the most senior pops first.
    let mut stack: Vec<(Node, i64)> = children
        .remove(&person)
        .unwrap_or_default()
        .into_iter()
        .rev()
        .map(|n| (n, 1))
        .collect();

    while let Some((node, depth)) = stack.pop() {
        if !visited.insert(node.id) {
            continue; // already emitted — we're in a cycle, stop this branch
        }
        if let Some(kids) = children.remove(&node.id) {
            for kid in kids.into_iter().rev() {
                stack.push((kid, depth + 1));
            }
        }
        out.push(Node { depth, ..node });
    }
    Ok(out)
}

/// Roots: people with no outgoing `reports_to` edge (nobody they report to).
/// Returned as depth-0 nodes, most senior first.
pub async fn roots(db: &Db) -> Result<Vec<Node>> {
    let sql = r#"
        SELECT p.id, p.name, p.title, 0 AS depth
        FROM people p
        WHERE NOT EXISTS (
          SELECT 1 FROM relationships r
          WHERE r.from_id = p.id AND r.kind = 'reports_to'
        );
    "#;
    let mut rows = db.conn().query(sql, ()).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Node {
            id: row.get(0)?,
            name: row.get(1)?,
            title: row.get(2)?,
            depth: row.get(3)?,
        });
    }
    out.sort_by_key(Node::sibling_key);
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
/// Two-space indent per level; siblings most-senior-first; titles shown so the
/// seniority ordering is legible.
pub async fn render_tree(db: &Db, person: i64) -> Result<String> {
    // Anchor line first.
    let anchor = crate::people::get_person(db, person)
        .await?
        .ok_or(crate::model::OrgError::PersonNotFound(person))?;

    // Show the id on every line: names aren't unique, so without ids two people
    // with the same name are indistinguishable in the tree.
    let mut out = String::new();
    out.push_str(&tree_line(anchor.id, &anchor.name, anchor.title.as_deref()));

    for node in subtree(db, person).await? {
        // depth 1 -> two spaces, depth 2 -> four, etc.
        for _ in 0..node.depth {
            out.push_str("  ");
        }
        out.push_str(&tree_line(node.id, &node.name, node.title.as_deref()));
    }
    Ok(out)
}

/// One `#id  Name — Title\n` line (title omitted when absent).
fn tree_line(id: i64, name: &str, title: Option<&str>) -> String {
    match title {
        Some(t) => format!("#{id}  {name} — {t}\n"),
        None => format!("#{id}  {name}\n"),
    }
}

/// Shared helper: run a `(id, name, title, depth)` query with one id param.
async fn collect_nodes(db: &Db, sql: &str, person: i64) -> Result<Vec<Node>> {
    let mut rows = db.conn().query(sql, [person]).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(Node {
            id: row.get(0)?,
            name: row.get(1)?,
            title: row.get(2)?,
            depth: row.get(3)?,
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
    async fn direct_reports_ordered_by_seniority() {
        let db = seeded().await;
        let reports = direct_reports(&db, 4).await.unwrap();
        let names: Vec<_> = reports.iter().map(|n| n.name.as_str()).collect();
        // Trent (Sr Engineer) > Jane (SWE II) > Mike (SWE I).
        assert_eq!(names, vec!["Trent Rush", "Jane Doe", "Mike Chen"]);
    }

    #[tokio::test]
    async fn subtree_ignores_other_edge_kinds() {
        let db = seeded().await;
        // A non-reporting edge must never pull someone into the subtree twice
        // or via the wrong path. Kinds stay open TEXT, so simulate one.
        crate::edges::relate(
            &db,
            crate::edges::RelateInput::manual(5, 6, "peer_review"),
        )
        .await
        .unwrap();

        let sub = subtree(&db, 4).await.unwrap();
        let names: Vec<_> = sub.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Trent Rush", "Jane Doe", "Mike Chen"]);
        assert!(sub.iter().all(|n| n.depth == 1));
    }

    #[tokio::test]
    async fn subtree_is_preorder_children_under_their_parent() {
        // Regression for the old (depth, name) flat ordering: a grandchild must
        // appear directly after their own boss, not after all of depth 1.
        let db = seeded().await;
        use crate::people::{add_person, PersonInput};
        // Give Trent (5) a report senior to Jane by name-order standards.
        let intern = add_person(
            &db,
            PersonInput {
                name: "Aaron Ali".into(),
                title: Some("Engineering Intern".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .id;
        crate::edges::set_boss(&db, intern, 5).await.unwrap();

        let sub = subtree(&db, 4).await.unwrap();
        let got: Vec<_> = sub.iter().map(|n| (n.name.as_str(), n.depth)).collect();
        // Aaron sits directly under Trent at depth 2, before Jane and Mike.
        assert_eq!(
            got,
            vec![
                ("Trent Rush", 1),
                ("Aaron Ali", 2),
                ("Jane Doe", 1),
                ("Mike Chen", 1),
            ]
        );
    }

    #[tokio::test]
    async fn roots_are_pat_then_dana_by_seniority() {
        let db = seeded().await;
        let r = roots(&db).await.unwrap();
        let names: Vec<_> = r.iter().map(|n| n.name.as_str()).collect();
        // Dana is a Director, Pat an Engineering Manager: Dana outranks Pat.
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
    async fn render_tree_indents_and_shows_titles() {
        let db = seeded().await;
        let text = render_tree(&db, 4).await.unwrap();
        // Pat (#4) at root; reports indented, most senior (Trent) first.
        assert!(text.starts_with("#4  Pat Smith — Engineering Manager\n"));
        let trent = text.find("#5  Trent Rush — Sr Engineer").unwrap();
        let jane = text.find("#6  Jane Doe — SWE II").unwrap();
        let mike = text.find("#7  Mike Chen — SWE I").unwrap();
        assert!(trent < jane && jane < mike);
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

        // Downward traversal: the visited set stops the loop, each node once.
        let sub = subtree(&db, a).await.unwrap();
        assert_eq!(sub.len(), 1); // just B — revisiting A is cut off
    }
}
