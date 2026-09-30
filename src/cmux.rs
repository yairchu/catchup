//! Finding the directories open in cmux, and opening views in it.
//!
//! `cmux tree` names each terminal's tty but not its working directory, so
//! directories come from the processes on those ttys.

use anyhow::{bail, Context, Result};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Command;

#[derive(Clone, Debug)]
pub struct Workspace {
    pub id: String,
    pub name: String,
}

pub fn inside_cmux() -> bool {
    std::env::var_os("CMUX_WORKSPACE_ID").is_some()
}

fn cmux(args: &[&str]) -> Result<String> {
    let out = Command::new("cmux")
        .args(args)
        .output()
        .context("running cmux")?;
    if !out.status.success() {
        bail!(
            "cmux {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every workspace with the working directories of the processes in its terminals.
pub fn workspace_dirs() -> Result<Vec<(Workspace, Vec<PathBuf>)>> {
    let workspaces = parse_tree(&cmux(&["tree", "--all"])?);
    let cwds = tty_cwds()?;
    Ok(workspaces
        .into_iter()
        .map(|(ws, ttys)| {
            let dirs: BTreeSet<PathBuf> = ttys
                .iter()
                .filter_map(|tty| cwds.get(tty))
                .flatten()
                .cloned()
                .collect();
            (ws, dirs.into_iter().collect())
        })
        .collect())
}

/// Parses `cmux tree --all` into workspaces and the ttys of their terminals.
fn parse_tree(tree: &str) -> Vec<(Workspace, Vec<String>)> {
    let mut result: Vec<(Workspace, Vec<String>)> = Vec::new();
    for line in tree.lines() {
        if let Some(rest) = line.split_once("workspace workspace:").map(|(_, r)| r) {
            let id = format!("workspace:{}", rest.split_whitespace().next().unwrap_or(""));
            let name = match (rest.find('"'), rest.rfind('"')) {
                (Some(a), Some(b)) if b > a => rest[a + 1..b].to_string(),
                _ => id.clone(),
            };
            result.push((Workspace { id, name }, Vec::new()));
        } else if let (Some(tty), Some((_, ttys))) = (
            line.split_whitespace().find_map(|w| w.strip_prefix("tty=")),
            result.last_mut(),
        ) {
            ttys.push(tty.to_string());
        }
    }
    result
}

/// Working directories of all processes, keyed by tty name (as in `ttys003`).
fn tty_cwds() -> Result<HashMap<String, BTreeSet<PathBuf>>> {
    let ps = Command::new("ps")
        .args(["-A", "-o", "pid=,tty="])
        .output()
        .context("running ps")?;
    let mut pid_tty = HashMap::new();
    for line in String::from_utf8_lossy(&ps.stdout).lines() {
        let mut words = line.split_whitespace();
        if let (Some(pid), Some(tty)) = (words.next(), words.next()) {
            if tty != "??" {
                let tty = if tty.starts_with("tty") { tty.to_string() } else { format!("tty{tty}") };
                pid_tty.insert(pid.to_string(), tty);
            }
        }
    }
    if pid_tty.is_empty() {
        return Ok(HashMap::new());
    }
    let pids: Vec<&str> = pid_tty.keys().map(String::as_str).collect();
    // lsof exits nonzero when some pids vanished meanwhile; its output is still good.
    let lsof = Command::new("lsof")
        .args(["-a", "-d", "cwd", "-Fn", "-p", &pids.join(",")])
        .output()
        .context("running lsof")?;
    let mut result: HashMap<String, BTreeSet<PathBuf>> = HashMap::new();
    let mut tty = None;
    for line in String::from_utf8_lossy(&lsof.stdout).lines() {
        if let Some(pid) = line.strip_prefix('p') {
            tty = pid_tty.get(pid);
        } else if let (Some(path), Some(tty)) = (line.strip_prefix('n'), tty) {
            result.entry(tty.clone()).or_default().insert(PathBuf::from(path));
        }
    }
    Ok(result)
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn shell_command(dir: &std::path::Path, program: &[&str]) -> String {
    let args: Vec<String> = program.iter().map(|a| shell_quote(a)).collect();
    // `exec` so that quitting the program also closes its split.
    format!("cd {} && exec {}", shell_quote(&dir.to_string_lossy()), args.join(" "))
}

/// Opens a split running `command`, returning its surface UUID when cmux reports one.
pub fn new_split(workspace: Option<&str>, command: &str) -> Result<Option<String>> {
    let mut args = vec!["new-split", "right", "--id-format", "uuids"];
    if let Some(ws) = workspace {
        args.extend(["--workspace", ws]);
    }
    args.extend(["--command", command]);
    let out = cmux(&args)?;
    Ok(out
        .split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .find(|w| w.len() == 36 && w.matches('-').count() == 4)
        .map(str::to_string))
}

pub fn close_surface(uuid: &str) {
    // The user may have already closed it; nothing to do then.
    let _ = cmux(&["close-surface", "--surface", uuid]);
}

pub fn select_workspace(workspace: &str) -> Result<()> {
    cmux(&["select-workspace", "--workspace", workspace]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tree() {
        let tree = r#"window window:1 [current] ◀ active
├── workspace workspace:11 "morning" [selected] ◀ active
│   └── pane pane:11 [focused] ◀ active
│       ├── surface surface:39 [terminal] "✳ Morning digest tool" tty=ttys028
│       └── surface surface:40 [terminal] "~/dev/morning" [selected] ◀ active ◀ here tty=ttys030
└── workspace workspace:5 "chopi & agentic infra"
    └── pane pane:5 [focused]
        └── surface surface:17 [terminal] "glog a17059c..eb18b86" [selected] tty=ttys005
"#;
        let parsed = parse_tree(tree);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0.id, "workspace:11");
        assert_eq!(parsed[0].0.name, "morning");
        assert_eq!(parsed[0].1, ["ttys028", "ttys030"]);
        assert_eq!(parsed[1].0.name, "chopi & agentic infra");
        assert_eq!(parsed[1].1, ["ttys005"]);
    }
}
