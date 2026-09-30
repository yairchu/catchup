use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

fn command(dir: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        // Never block on a credential prompt: catchup runs many fetches unattended.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    cmd
}

fn run(mut cmd: Command, args: &[&str]) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    run(command(dir, args), args)
}

/// Like `git`, for queries where failure just means "no such thing".
pub fn query(dir: &Path, args: &[&str]) -> Option<String> {
    git(dir, args).ok().filter(|s| !s.is_empty())
}

pub fn fetch(dir: &Path, remote: &str) -> Result<()> {
    let args = ["fetch", "--quiet", remote];
    let mut cmd = command(dir, &args);
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    run(cmd, &args).map(drop)
}

pub fn rev(dir: &Path, refname: &str) -> Option<String> {
    query(dir, &["rev-parse", "--verify", "--quiet", &format!("{refname}^{{commit}}")])
}

pub fn is_ancestor(dir: &Path, a: &str, b: &str) -> bool {
    command(dir, &["merge-base", "--is-ancestor", a, b])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
