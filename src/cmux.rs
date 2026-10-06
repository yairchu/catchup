//! Finding the directories open in cmux, and opening views in it.
//!
//! Each terminal's directory comes from cmux, which knows it even for terminals
//! restored after a restart whose shells haven't started yet. Shells may have
//! changed directory since, so the directories of the processes on each
//! terminal's tty are added too.

use anyhow::{bail, Context, Result};
use serde_json::Value;
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

/// Every workspace with the working directories of the terminals in it.
pub fn workspace_dirs() -> Result<Vec<(Workspace, Vec<PathBuf>)>> {
    let workspaces = parse_tree(&cmux(&["tree", "--all"])?);
    // `cmux tree` reports the tty a not-yet-started terminal had before a
    // restart, which by now may belong to another terminal. Such a tty shows
    // up under more than one surface, so it can't say whose processes are on it.
    let mut tty_count: HashMap<&str, usize> = HashMap::new();
    for (_, surfaces) in &workspaces {
        for tty in surfaces.iter().filter_map(|s| s.tty.as_deref()) {
            *tty_count.entry(tty).or_default() += 1;
        }
    }
    let cwds = tty_cwds()?;
    workspaces
        .iter()
        .map(|(ws, surfaces)| {
            let mut dirs = surface_dirs(&ws.id)?;
            dirs.extend(
                surfaces
                    .iter()
                    .filter_map(|s| s.tty.as_deref())
                    .filter(|tty| tty_count[tty] == 1)
                    .filter_map(|tty| cwds.get(tty))
                    .flatten()
                    .cloned(),
            );
            Ok((ws.clone(), dirs.into_iter().collect()))
        })
        .collect()
}

/// The directories cmux has for the terminals of a workspace.
fn surface_dirs(workspace: &str) -> Result<BTreeSet<PathBuf>> {
    let params = serde_json::json!({ "workspace_id": workspace }).to_string();
    let out = cmux(&["rpc", "surface.list", &params])?;
    parse_surface_list(workspace, &out)
}

fn parse_surface_list(workspace: &str, out: &str) -> Result<BTreeSet<PathBuf>> {
    let list: Value = serde_json::from_str(out).context("parsing cmux surface.list")?;
    let listed = list["workspace_ref"].as_str();
    if listed != Some(workspace) {
        bail!("cmux surface.list for {workspace} listed {}", listed.unwrap_or("nothing"));
    }
    Ok(list["surfaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["requested_working_directory"].as_str())
        .map(PathBuf::from)
        .collect())
}

struct Surface {
    tty: Option<String>,
}

/// Parses `cmux tree --all` into workspaces and their terminals.
fn parse_tree(tree: &str) -> Vec<(Workspace, Vec<Surface>)> {
    let mut result: Vec<(Workspace, Vec<Surface>)> = Vec::new();
    for line in tree.lines() {
        if let Some(rest) = line.split_once("workspace workspace:").map(|(_, r)| r) {
            let id = format!("workspace:{}", rest.split_whitespace().next().unwrap_or(""));
            let name = match (rest.find('"'), rest.rfind('"')) {
                (Some(a), Some(b)) if b > a => rest[a + 1..b].to_string(),
                _ => id.clone(),
            };
            result.push((Workspace { id, name }, Vec::new()));
        } else if let (true, Some((_, surfaces))) =
            (line.contains("[terminal]"), result.last_mut())
        {
            let tty = line.split_whitespace().find_map(|w| w.strip_prefix("tty="));
            surfaces.push(Surface { tty: tty.map(str::to_string) });
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

/// Opens a split running `command`.
pub fn new_split(workspace: Option<&str>, command: &str) -> Result<()> {
    let mut args = vec!["new-split", "right"];
    if let Some(ws) = workspace {
        args.extend(["--workspace", ws]);
    }
    args.extend(["--command", command]);
    cmux(&args).map(drop)
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
        let ttys = |i: usize| -> Vec<_> { parsed[i].1.iter().map(|s| s.tty.clone()).collect() };
        assert_eq!(ttys(0), [Some("ttys028".into()), Some("ttys030".into())]);
        assert_eq!(parsed[1].0.name, "chopi & agentic infra");
        assert_eq!(ttys(1), [Some("ttys005".into())]);
    }

    #[test]
    fn parses_surface_list() {
        let out = r#"{
  "surfaces" : [
    { "ref" : "surface:3", "requested_working_directory" : "/a", "type" : "terminal" },
    { "ref" : "surface:4", "requested_working_directory" : null, "type" : "browser" },
    { "ref" : "surface:5", "requested_working_directory" : "/a", "type" : "terminal" }
  ],
  "workspace_ref" : "workspace:7"
}"#;
        let dirs = parse_surface_list("workspace:7", out).unwrap();
        assert_eq!(dirs.into_iter().collect::<Vec<_>>(), [PathBuf::from("/a")]);
        assert!(parse_surface_list("workspace:8", out).is_err());
    }
}
