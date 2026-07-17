//! IPC commands: thin async wrappers over org-core, one per UI need.
//!
//! Errors cross the IPC boundary as strings — the webview can only display
//! them, so the Display form of OrgError is exactly the right payload.

use org_core::{Db, Node, Person, SearchHit};
use serde::Serialize;
use tauri::State;

/// Everything the person detail pane needs, in one round trip.
#[derive(Serialize)]
pub struct WhoView {
    pub person: Person,
    /// Nearest boss first (depth 1..), like `org who`.
    pub chain: Vec<Node>,
    /// Most senior first.
    pub reports: Vec<Node>,
}

/// The tree pane: the anchor person plus their subtree in pre-order.
#[derive(Serialize)]
pub struct TreeView {
    pub anchor: Person,
    pub nodes: Vec<Node>,
}

/// Map core errors to the string the UI shows.
fn err(e: org_core::OrgError) -> String {
    e.to_string()
}

/// Fuzzy search; empty query returns everyone (that's the app's initial list).
#[tauri::command]
pub async fn search(db: State<'_, Db>, query: String) -> Result<Vec<SearchHit>, String> {
    org_core::fuzzy_search(&db, &query).await.map_err(err)
}

/// Person detail + chain of command + direct reports.
#[tauri::command]
pub async fn who(db: State<'_, Db>, id: i64) -> Result<WhoView, String> {
    let person = org_core::get_person(&db, id)
        .await
        .map_err(err)?
        .ok_or_else(|| format!("no person with id {id}"))?;
    let chain = org_core::chain_of_command(&db, id).await.map_err(err)?;
    let reports = org_core::direct_reports(&db, id).await.map_err(err)?;
    Ok(WhoView {
        person,
        chain,
        reports,
    })
}

/// The reporting subtree under a person.
#[tauri::command]
pub async fn tree(db: State<'_, Db>, id: i64) -> Result<TreeView, String> {
    let anchor = org_core::get_person(&db, id)
        .await
        .map_err(err)?
        .ok_or_else(|| format!("no person with id {id}"))?;
    let nodes = org_core::subtree(&db, id).await.map_err(err)?;
    Ok(TreeView { anchor, nodes })
}
