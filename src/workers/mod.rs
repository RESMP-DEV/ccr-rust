use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

mod render;
mod report;
mod stream;

#[cfg(test)]
#[path = "selection_tests.rs"]
mod selection_tests;

#[derive(Args)]
pub struct WorkerArgs {
    /// Saved launcher artifacts (default: ~/.cache/ccr-worker-runs)
    #[arg(long, global = true, env = "CCR_WORKER_RUNS_DIR")]
    runs_dir: Option<PathBuf>,
    /// Emit one bounded JSON document instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: WorkerCommand,
}

#[derive(Subcommand)]
enum WorkerCommand {
    /// Summarize recent runs; never print tool output or final reports
    List {
        #[arg(long, default_value = "10", value_parser = parse_limit)]
        limit: usize,
    },
    /// Show usage, failures, recent activity and a bounded final report
    Show {
        /// Run ID, run directory, receipt path, or latest
        #[arg(default_value = "latest")]
        run: String,
        #[arg(long, default_value = "1200", value_parser = parse_chars)]
        max_chars: usize,
    },
    /// Read a page of compact events; reuse next_cursor with --after
    Events {
        #[arg(default_value = "latest")]
        run: String,
        /// Byte cursor returned by a previous events page for this run
        #[arg(long, default_value = "0")]
        after: u64,
        #[arg(long, default_value = "20", value_parser = parse_limit)]
        limit: usize,
        /// Filter by event type or item type (e.g. command_execution)
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value = "240", value_parser = parse_chars)]
        max_chars: usize,
    },
    /// Read only one event's tool output or message, by its byte cursor
    Detail {
        run: String,
        /// Event cursor from show/events (unique even across resumed turns)
        cursor: u64,
        #[arg(long, default_value = "4000", value_parser = parse_chars)]
        max_chars: usize,
    },
}

fn bounded_number(value: &str, max: usize) -> std::result::Result<usize, String> {
    let number = value.parse::<usize>().map_err(|error| error.to_string())?;
    if number == 0 || number > max {
        return Err(format!("must be between 1 and {max}"));
    }
    Ok(number)
}

fn parse_limit(value: &str) -> std::result::Result<usize, String> {
    bounded_number(value, 100)
}

fn parse_chars(value: &str) -> std::result::Result<usize, String> {
    bounded_number(value, 16000)
}

pub fn run(args: WorkerArgs) -> Result<()> {
    let root = match args.runs_dir {
        Some(root) => root,
        None => dirs::home_dir()
            .context("no home directory; pass --runs-dir")?
            .join(".cache/ccr-worker-runs"),
    };
    let output = match args.command {
        WorkerCommand::List { limit } => {
            let paths = run_directories(&root)?;
            let mut runs = Vec::new();
            for path in paths.iter().take(limit) {
                runs.push(report::list_entry(path)?);
            }
            serde_json::json!({"schema_version": 1, "runs": runs,
                "total_runs": paths.len(), "omitted_runs": paths.len().saturating_sub(limit)})
        }
        WorkerCommand::Show { run, max_chars } => {
            to_value(report::summarize(&resolve_run(&root, &run)?, max_chars)?)?
        }
        WorkerCommand::Events {
            run,
            after,
            limit,
            kind,
            max_chars,
        } => to_value(report::events(
            &resolve_run(&root, &run)?,
            after,
            limit,
            kind.as_deref(),
            max_chars,
        )?)?,
        WorkerCommand::Detail {
            run,
            cursor,
            max_chars,
        } => to_value(report::detail(
            &resolve_run(&root, &run)?,
            cursor,
            max_chars,
        )?)?,
    };
    let mut stdout = std::io::stdout().lock();
    if args.json {
        use std::io::Write;
        serde_json::to_writer(&mut stdout, &output)?;
        writeln!(stdout)?;
    } else {
        render::write(&mut stdout, &output)?;
    }
    Ok(())
}

fn to_value(value: impl Serialize) -> Result<serde_json::Value> {
    Ok(serde_json::to_value(value)?)
}

fn run_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read worker runs directory"),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let path = entry.path();
            if ["events.jsonl", "status.json", "final.txt", "stderr.txt"]
                .iter()
                .any(|name| path.join(name).exists())
            {
                paths.push(path);
            }
        }
    }
    paths.sort_by(|left, right| right.file_name().cmp(&left.file_name()));
    Ok(paths)
}

fn resolve_run(root: &Path, run: &str) -> Result<PathBuf> {
    let path = if run == "latest" {
        run_directories(root)?
            .into_iter()
            .next()
            .context("no worker runs found")?
    } else if Path::new(run).exists() {
        PathBuf::from(run)
    } else {
        root.join(run)
    };
    let path = if path.is_file() {
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let receipt = report::read_receipt(&path)?;
            let run_dir = receipt["run_dir"]
                .as_str()
                .context("receipt has no run_dir")?;
            PathBuf::from(run_dir)
        } else if path.file_name().is_some_and(|name| name == "events.jsonl") {
            path.parent().context("trace has no parent")?.to_path_buf()
        } else {
            bail!("expected a run directory, events.jsonl or JSON receipt");
        }
    } else {
        path
    };
    if !path.is_dir() {
        bail!("worker run not found: {}", path.display());
    }
    Ok(path.canonicalize()?)
}
