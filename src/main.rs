mod cmux;
mod git;
mod github;
mod scan;
mod tui;

use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;

const USAGE: &str = "\
catchup — fetch every repo open in cmux and browse what others pushed

Usage: catchup [--print] [--no-fetch] [DIR...]

Without DIRs, the repos are the ones open in cmux's terminals.
Shows the default branch and the checked-out branches of each repo,
with what this run's fetch brought in.

Options:
  --print     Print a summary instead of opening the TUI
  --no-fetch  Skip fetching, only show what can be pulled
  -h, --help  Print help";

/// One repository to scan, with the cmux workspace it was found in.
pub struct Target {
    pub worktrees: Vec<PathBuf>,
    pub workspace: Option<cmux::Workspace>,
}

fn main() -> Result<()> {
    let mut print = false;
    let mut fetch = true;
    let mut dirs = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--print" => print = true,
            "--no-fetch" => fetch = false,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ if arg.starts_with('-') => anyhow::bail!("unknown option {arg}\n\n{USAGE}"),
            _ => dirs.push(PathBuf::from(arg)),
        }
    }
    let found = if dirs.is_empty() {
        cmux::workspace_dirs()?
            .into_iter()
            .flat_map(|(ws, dirs)| dirs.into_iter().map(move |d| (Some(ws.clone()), d)))
            .collect()
    } else {
        dirs.into_iter().map(|d| (None, d)).collect()
    };
    let targets = group(found);
    if targets.is_empty() {
        anyhow::bail!("no git repositories found");
    }

    let (tx, rx) = mpsc::channel();
    for (i, t) in targets.iter().enumerate() {
        let tx = tx.clone();
        let worktrees = t.worktrees.clone();
        std::thread::spawn(move || {
            let _ = tx.send((i, scan::scan(&worktrees, fetch)));
        });
    }
    if print {
        drop(tx);
        let mut repos: Vec<Option<scan::Repo>> = targets.iter().map(|_| None).collect();
        for (i, repo) in rx {
            repos[i] = Some(repo);
        }
        tui::print(&repos.into_iter().flatten().collect::<Vec<_>>());
        Ok(())
    } else {
        tui::run(targets, fetch, tx, rx)
    }
}

/// Groups directories by repository, keeping the order they were found in.
fn group(found: Vec<(Option<cmux::Workspace>, PathBuf)>) -> Vec<Target> {
    let mut targets: Vec<Target> = Vec::new();
    let mut by_common_dir: HashMap<PathBuf, usize> = HashMap::new();
    for (ws, dir) in found {
        let Some((toplevel, common)) = scan::locate(&dir) else {
            continue;
        };
        match by_common_dir.get(&common) {
            Some(&i) => {
                if !targets[i].worktrees.contains(&toplevel) {
                    targets[i].worktrees.push(toplevel);
                }
            }
            None => {
                by_common_dir.insert(common, targets.len());
                targets.push(Target {
                    worktrees: vec![toplevel],
                    workspace: ws,
                });
            }
        }
    }
    targets
}
