//! Top-level command dispatch, moved out of the `gx` bin's `main.rs` (Track
//! B0, Phase 3) so the bin stays a thin shim: parse args, set up logging,
//! intercept `mcp`, then hand off here.

use crate::cli::{Cli, Commands};
use crate::{checkout, cleanup, clone, create, doctor, review, rollback, status, undo};
use eyre::Result;
use local::config::Config;
use log::info;

/// Dispatch a parsed [`Cli`] invocation against the loaded [`Config`]. The
/// `mcp` subcommand is intercepted by the bin before this is ever called.
pub fn run_application(cli: &Cli, config: &Config) -> Result<()> {
    info!("Starting gx with command: {:?}", cli.command);

    match &cli.command {
        Commands::Status {
            detailed,
            no_emoji,
            no_color,
            patterns,
            fetch_first,
            no_remote,
        } => {
            let options = status::StatusCommandOptions {
                detailed: *detailed,
                use_emoji: !no_emoji,
                use_colors: !no_color,
                patterns,
                fetch_first: *fetch_first,
                no_remote: *no_remote,
            };
            status::process_status_command(cli, config, options)
        }
        Commands::Checkout {
            create_branch,
            from_branch,
            branch_name,
            stash,
            patterns,
        } => checkout::process_checkout_command(
            cli,
            config,
            *create_branch,
            from_branch.as_deref(),
            branch_name,
            *stash,
            patterns,
        ),
        Commands::Clone {
            user_or_org,
            include_archived,
            patterns,
        } => clone::process_clone_command(cli, config, user_or_org, *include_archived, patterns),
        Commands::Create {
            files,
            change_id,
            patterns,
            commit,
            pr,
            draft,
            yes,
            report,
            action,
        } => match action {
            None => create::show_matches(cli, config, files, patterns),
            Some(action) => {
                let propose_only =
                    matches!(action, crate::cli::CreateAction::Llm { propose, .. } if *propose);
                let change = match action {
                    crate::cli::CreateAction::Add { path, content } => {
                        create::Change::Add(path.clone(), content.clone())
                    }
                    crate::cli::CreateAction::Delete => create::Change::Delete,
                    crate::cli::CreateAction::Sub {
                        pattern,
                        replacement,
                    } => create::Change::Sub(pattern.clone(), replacement.clone()),
                    crate::cli::CreateAction::Regex {
                        pattern,
                        replacement,
                    } => create::Change::Regex(pattern.clone(), replacement.clone()),
                    crate::cli::CreateAction::Llm { prompt, .. } => {
                        create::Change::Llm(prompt.clone())
                    }
                };
                create::process_create_command(
                    cli,
                    config,
                    files,
                    change_id.clone(),
                    patterns,
                    commit.clone(),
                    *pr,
                    *draft,
                    *yes,
                    change,
                    propose_only,
                    report.as_deref(),
                )
            }
        },
        Commands::Apply {
            change_id,
            pr,
            draft,
            yes,
        } => create::process_apply_command(cli, config, change_id, *pr, *draft, *yes),
        Commands::Review {
            org,
            patterns,
            action,
        } => match action {
            crate::cli::ReviewAction::Ls { change_ids } => {
                review::process_review_ls_command(cli, config, org.as_deref(), patterns, change_ids)
            }
            crate::cli::ReviewAction::Clone { change_id, all } => {
                review::process_review_clone_command(
                    cli,
                    config,
                    org.as_deref(),
                    patterns,
                    change_id,
                    *all,
                )
            }
            crate::cli::ReviewAction::Approve {
                change_id,
                admin,
                auto,
                yes,
            } => review::process_review_approve_command(
                cli,
                config,
                org.as_deref(),
                patterns,
                change_id,
                *admin,
                *auto,
                *yes,
            ),
            crate::cli::ReviewAction::Delete { change_id, yes } => {
                review::process_review_delete_command(
                    cli,
                    config,
                    org.as_deref(),
                    patterns,
                    change_id,
                    *yes,
                )
            }
            crate::cli::ReviewAction::Sync { change_id } => review::process_review_sync_command(
                cli,
                config,
                org.as_deref(),
                patterns,
                change_id,
            ),
            crate::cli::ReviewAction::Purge { yes } => {
                review::process_review_purge_command(cli, config, org.as_deref(), patterns, *yes)
            }
        },
        Commands::Rollback { action } => rollback::handle_rollback(action.clone()),
        Commands::Undo {
            change_id,
            org,
            yes,
        } => undo::process_undo_command(cli, config, change_id, org.as_deref(), *yes),
        Commands::Cleanup {
            change_id,
            all,
            list,
            include_remote,
            force,
            yes,
        } => cleanup::process_cleanup_command(
            cli,
            config,
            change_id.as_deref(),
            *all,
            *list,
            *include_remote,
            *force,
            *yes,
        ),
        Commands::Doctor { purge } => doctor::run_doctor(*purge),
        // Intercepted in the bin's `run()` before `run_application` is ever
        // called, so it never reaches this dispatch.
        Commands::Mcp(_) => unreachable!("mcp is handled before run_application"),
    }
}
