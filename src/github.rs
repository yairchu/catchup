//! What GitHub knows about a branch, through the `gh` CLI.

use crate::git::query;
use std::path::Path;
use std::process::{Command, Stdio};

/// The base branch of the open pull request from `branch` on `remote`, if any.
///
/// Git doesn't record which branch another one was started from, but a PR
/// states what its author means it to go into.
pub fn pr_base(dir: &Path, remote: &str, branch: &str) -> Option<String> {
    let url = query(dir, &["config", "--get", &format!("remote.{remote}.url")])?;
    let (owner, repo) = repo_of(&url)?;
    let out = Command::new("gh")
        .args(["api", "-X", "GET", &format!("repos/{owner}/{repo}/pulls")])
        .args(["-f", &format!("head={owner}:{branch}"), "-f", "state=open"])
        .args(["--jq", ".[0].base.ref // empty"])
        .env("GH_PROMPT_DISABLED", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!base.is_empty()).then_some(base)
}

/// Owner and name of a GitHub repository from its remote URL.
fn repo_of(url: &str) -> Option<(&str, &str)> {
    let url = url.trim_end_matches('/');
    let url = url.strip_suffix(".git").unwrap_or(url);
    let (_, path) = url.split_once("github.com")?;
    let path = path.strip_prefix(':').or_else(|| path.strip_prefix('/'))?;
    match path.split('/').collect::<Vec<_>>()[..] {
        [owner, repo] if !owner.is_empty() && !repo.is_empty() => Some((owner, repo)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_urls() {
        for url in [
            "git@github.com:yairchu/catchup.git",
            "ssh://git@github.com/yairchu/catchup",
            "https://github.com/yairchu/catchup.git",
            "https://user@github.com/yairchu/catchup/",
        ] {
            assert_eq!(repo_of(url), Some(("yairchu", "catchup")), "{url}");
        }
        assert_eq!(repo_of("https://gitlab.com/yairchu/catchup.git"), None);
        assert_eq!(repo_of("https://github.com/yairchu"), None);
    }
}
