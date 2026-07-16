//! `org` CLI — thin layer over `org-core`. Parses args, opens the DB, dispatches
//! to core, and prints. All logic lives in core; this file is wiring + output.

mod render;
mod update;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use org_core::{Config, Db, PersonInput};
use std::path::PathBuf;

/// Relationship-aware employee directory.
#[derive(Parser)]
#[command(name = "org", version, about)]
struct Cli {
    /// Use this database file instead of $ORG_DB / ~/.org/org.db.
    #[arg(long, global = true)]
    file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add a person.
    Add {
        name: String,
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        notes: Option<String>,
        /// After adding, try to infer a boss from teammates' reporting lines.
        #[arg(long)]
        infer_boss: bool,
    },
    /// Fuzzy-search people by name/team/title/notes.
    Find { query: String },
    /// List people, optionally filtered by team.
    List {
        #[arg(long)]
        team: Option<String>,
    },
    /// Show per-team headcounts.
    Teams,
    /// Print the reporting tree under a person (by id).
    Tree { id: i64 },
    /// Show a person's detail plus chain of command and direct reports.
    Who { id: i64 },
    /// Set a person's boss (replaces any existing reporting line).
    SetBoss {
        /// The report.
        person: i64,
        /// Their new boss.
        boss: i64,
    },
    /// Remove a person (cascades their relationship edges).
    Remove { id: i64 },
    /// Dump the whole directory as JSON to stdout.
    Export,
    /// Load a JSON dump from a file (or '-' for stdin).
    Import { path: String },
    /// Update `org` itself. By default pulls the latest from GitHub and reinstalls.
    Update {
        /// Install from a local checkout instead of GitHub.
        #[arg(long)]
        local: bool,
        /// Local checkout path (implies --local). Defaults to $ORG_SRC, then the
        /// repo this binary was built from (recorded at compile time).
        #[arg(long)]
        source: Option<PathBuf>,
        /// Print the install command instead of running it.
        #[arg(long)]
        dry_run: bool,
        /// Only check whether an update is available; don't install it.
        #[arg(long)]
        check: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // `update` manages the binary itself — it must not require (or be blocked
    // by) the database, so handle it before opening any connection.
    if let Command::Update {
        local,
        source,
        dry_run,
        check,
    } = &cli.command
    {
        return update::run(*local, source.clone(), *dry_run, *check);
    }

    // Resolve DB location with the precedence core defines, then open it.
    let config = Config::resolve(cli.file);
    let db = Db::open(config)
        .await
        .context("opening the org database")?;

    match cli.command {
        Command::Add {
            name,
            team,
            title,
            notes,
            infer_boss,
        } => {
            let person = org_core::add_person(
                &db,
                PersonInput {
                    name,
                    team,
                    title,
                    notes,
                },
            )
            .await?;
            print!("{}", render::person_detail(&person));

            if infer_boss {
                let inf = org_core::infer_boss(&db, person.id).await?;
                print!("{}", render::inference(&person.name, &inf));
            }
        }

        Command::Find { query } => {
            let hits = org_core::fuzzy_search(&db, &query).await?;
            print!("{}", render::search_results(&hits));
        }

        Command::List { team } => {
            let people = org_core::list_people(&db, team.as_deref()).await?;
            print!("{}", render::person_table(&people));
        }

        Command::Teams => {
            let counts = org_core::tree::team_headcounts(&db).await?;
            print!("{}", render::teams(&counts));
        }

        Command::Tree { id } => {
            // render_tree errors if the id is unknown — surface it cleanly.
            let text = org_core::tree::render_tree(&db, id)
                .await
                .with_context(|| format!("rendering tree for person {id}"))?;
            print!("{text}");
        }

        Command::Who { id } => {
            let person = org_core::get_person(&db, id)
                .await?
                .with_context(|| format!("no person with id {id}"))?;
            print!("{}", render::person_detail(&person));

            let chain = org_core::tree::chain_of_command(&db, id).await?;
            println!("\nChain of command:");
            print!("{}", render::chain(&chain));

            let reports = org_core::tree::direct_reports(&db, id).await?;
            println!("\nDirect reports:");
            if reports.is_empty() {
                println!("  (none)");
            } else {
                // Already most-senior-first from core.
                for r in &reports {
                    match &r.title {
                        Some(t) => println!("  - #{}  {} — {}", r.id, r.name, t),
                        None => println!("  - #{}  {}", r.id, r.name),
                    }
                }
            }
        }

        Command::SetBoss { person, boss } => {
            org_core::set_boss(&db, person, boss).await?;
            println!("Set boss: #{person} now reports to #{boss}.");
        }

        Command::Remove { id } => {
            org_core::remove_person(&db, id).await?;
            println!("Removed person #{id} (and any of their relationship edges).");
        }

        Command::Export => {
            let json = org_core::export_json(&db).await?;
            println!("{json}");
        }

        Command::Import { path } => {
            // '-' reads stdin; otherwise read the named file.
            let json = if path == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("reading dump from stdin")?;
                buf
            } else {
                std::fs::read_to_string(&path)
                    .with_context(|| format!("reading dump file {path}"))?
            };
            org_core::import_json(&db, &json).await?;
            println!("Imported directory from {path}.");
        }

        // Handled above, before the DB is opened. Unreachable here.
        Command::Update { .. } => unreachable!("update is dispatched before db open"),
    }

    Ok(())
}
