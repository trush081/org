# org

A relationship-aware employee directory. Look up a person or team and see their
role, chain of command, who reports to them, and how they relate to others.
Reporting lines are just one kind of relationship in a graph — which leaves room
to grow into mentorship, collaboration, and (later) AI-inferred hierarchy and
semantic search over notes.

CLI first; a Tauri desktop app and AI features come later, over the same core.

## Status

**Milestone 1 complete:** `core` + `cli`, working and tested.

```
cargo test --workspace                                  # 38 passing
cargo clippy --workspace --all-targets -- -D warnings   # clean
```

## Quick start

```sh
cargo build
alias org=./target/debug/org   # or `cargo run -p org -- <args>`

org add "Pat Smith"  --team "IDS Fulfillment" --title "Eng Manager"
org add "Trent Rush" --team "IDS Fulfillment" --title "Sr Engineer"
org set-boss 2 1                 # Trent (#2) reports to Pat (#1)
org add "Mike Chen" --team "IDS Fulfillment" --infer-boss   # guesses Pat from teammates
org who 2                        # detail + chain of command + direct reports
org tree 1                       # the reporting tree under Pat
```

## Commands

| Command | What it does |
|---|---|
| `add <name> [--team] [--title] [--notes] [--infer-boss]` | Add a person; optionally infer a boss from teammates. |
| `find <query>` | Fuzzy search across name/team/title/notes (substring + typo tolerance). |
| `list [--team <team>]` | List people, optionally one team. |
| `teams` | Per-team headcounts. |
| `tree <id>` | Indented reporting tree under a person. |
| `who <id>` | Person detail + chain of command + direct reports. |
| `set-boss <person> <boss>` | Set/replace a reporting line. |
| `relate <from> <to> --kind <kind>` | Generic edge, e.g. `--kind mentors`. |
| `remove <id>` | Delete a person (cascades their edges). |
| `export` | Dump the whole directory as JSON to stdout. |
| `import <file\|->` | Load a JSON dump from a file or stdin. |

## Database

The directory lives in a local [libSQL](https://github.com/tursodatabase/libsql)
file (SQLite-compatible). Location is resolved as:

1. `--file <path>` flag, else
2. `$ORG_DB` env var, else
3. `~/.org/org.db` (default; created on first use).

libSQL was chosen over rusqlite so the same data layer and SQL can later sync to
a remote/cloud database (Turso) as a config change, not a rewrite. No cloud sync
is built today — the connection layer just doesn't block it.

### Schema — an edges-only graph

```
people          (id, name, team, title, notes, created_at, updated_at)
relationships   (id, from_id, to_id, kind, notes, confidence, source, created_at)
```

There is **no `boss_id` column.** The hierarchy is `reports_to` rows in
`relationships` (`from_id` reports to `to_id`) — one relationship kind among
many, so future features treat it uniformly. `confidence` (0..1) and `source`
(`manual` | `inferred` | `imported`) let AI-inferred edges coexist with human
truth and be filtered or reviewed; manual entries default to `1.0` / `manual`.

Indexes `(from_id, kind)` and `(to_id, kind)` cover the traversal queries, which
always pair an id with a kind filter. The schema is created at startup with
`CREATE TABLE IF NOT EXISTS` — no migration framework yet (nothing to upgrade).

## Architecture

A Cargo workspace from the start, so logic stays UI-agnostic:

- **`core/`** (`org-core`) — library crate: data model, persistence, all logic.
  The shared brain; no UI/CLI assumptions.
- **`cli/`** (`org`) — binary crate (clap) over `core`. Wiring + pretty output only.
- **`app/`** — Tauri desktop app, *later*, over the same `core`.

### `core` modules

| Module | Responsibility |
|---|---|
| `model` | `Person`, `Relationship`, `OrgError`. |
| `db` | libSQL connection, config resolution, embedded schema. The only module that knows about libSQL. |
| `people` | Person CRUD. |
| `edges` | `relate` (set/replace by kind), `set_boss` (delete-then-insert — never accumulates stale lines), `unrelate`. |
| `infer` | Boss inference. `tally_boss_vote` is a pure, DB-free function — the swap point for a real model later. |
| `search` | Fuzzy search: substring + Levenshtein (budget scales with query length), ranked name > team > title > notes. |
| `tree` | Reporting-line traversals via recursive CTEs (chain of command, direct reports, subtree, roots, headcounts, tree render). Every recursive query is cycle-guarded with `depth < 100`. |
| `io` | JSON export/import (id-preserving, idempotent). |

### Boss inference

Adding someone to a team with no reporting line can infer one from teammates'
bosses: requires **≥2 votes** and a **≥50% plurality**, never infers someone as
their own boss, and never overwrites an existing line. The inferred edge is
written with `source='inferred'` and `confidence` = the vote share. This is the
seed of the future AI work; the rule lives behind a clean function so it can be
swapped or augmented with a model.

## Development

```sh
cargo test --workspace                                  # all tests
cargo clippy --workspace --all-targets -- -D warnings   # lint
cargo run -p org -- <args>                              # run the CLI
```

Tests run against in-memory libSQL databases, so they're isolated and fast.

## Roadmap (deliberately later)

- **Semantic search** over notes (e.g. `sqlite-vec`) — "who knows about X."
  Slots into `search` as a second ranked source.
- **AI hierarchy building** — natural-language entry and inference from signals,
  writing `inferred` edges with sub-1.0 confidence. `infer::tally_boss_vote` is
  the hook.
- **Tauri desktop GUI** (`app/`) — directory, search, clickable org chart, over
  the same `core`.
