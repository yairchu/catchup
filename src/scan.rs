//! What a fetch brought into one repository.

use crate::git::{self, git, query, rev};
use crate::github;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mine {
    No,
    /// Authored and committed by the user: something they pushed themselves.
    Pushed,
    /// Authored by the user but committed by someone (or something) else,
    /// such as a PR that got merged, squashed or rebased.
    Landed,
}

#[derive(Clone)]
pub struct Commit {
    pub short: String,
    pub author: String,
    pub when: String,
    pub subject: String,
    pub mine: Mine,
}

#[derive(Clone)]
pub struct Branch {
    /// Local branch name, e.g. `main`.
    pub name: String,
    /// Remote-tracking ref, e.g. `refs/remotes/origin/main`.
    pub remote_ref: String,
    pub is_default: bool,
    /// Tip before this run's fetch, and after it.
    pub old: Option<String>,
    pub new: Option<String>,
    /// The branch this one is meant to go into, as in `origin/feature`, when
    /// its open pull request names one other than the default branch.
    pub base: Option<String>,
    /// Tips whose history is not new here: the old tip, and for branches
    /// other than the default one, their base or else the default branch.
    pub exclude: Vec<String>,
    pub commits: Vec<Commit>,
    /// The fetch moved the branch to something not containing its old tip.
    pub rewritten: bool,
    pub local: Option<Local>,
    /// Outcome of the last pull attempt.
    pub note: Option<String>,
}

#[derive(Clone)]
pub struct Local {
    pub sha: String,
    pub checked_out_in: Option<PathBuf>,
    pub ahead: usize,
    pub behind: usize,
}

impl Branch {
    pub fn remote_short(&self) -> &str {
        self.remote_ref.strip_prefix("refs/remotes/").unwrap_or(&self.remote_ref)
    }

    /// Something happened in this fetch, or there is something to pull.
    pub fn interesting(&self) -> bool {
        !self.commits.is_empty() || self.rewritten || self.local.as_ref().is_some_and(|l| l.behind > 0)
    }

    /// Revisions for `git log` showing what is new on this branch.
    pub fn log_range(&self) -> Option<Vec<String>> {
        let new = short(self.new.as_deref()?);
        if self.commits.is_empty() {
            // Nothing new in this fetch: show what is not pulled yet instead.
            let local = short(&self.local.as_ref().filter(|l| l.behind > 0)?.sha);
            return Some(vec![format!("{local}..{new}")]);
        }
        match &self.exclude[..] {
            [old] => Some(vec![format!("{}..{new}", short(old))]),
            exclude => Some(
                std::iter::once(new.to_string())
                    .chain(exclude.iter().map(|e| format!("^{}", short(e))))
                    .collect(),
            ),
        }
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

pub struct Repo {
    pub path: PathBuf,
    pub display: String,
    pub branches: Vec<Branch>,
    /// Fetch failures; the branches still show what git already knew.
    pub error: Option<String>,
}

/// The repositories a set of directories belongs to, as (main path, git common dir).
pub fn locate(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = query(dir, &["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-common-dir"])?;
    let mut lines = out.lines();
    Some((PathBuf::from(lines.next()?), PathBuf::from(lines.next()?)))
}

pub fn display_path(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap().display())
        }
        _ => path.display().to_string(),
    }
}

/// Snapshot, fetch, and compare. `worktrees` are the checkouts open in cmux;
/// the first one is where git commands run.
pub fn scan(worktrees: &[PathBuf], fetch: bool) -> Repo {
    let path = worktrees[0].clone();
    let mut repo = Repo {
        display: display_path(&path),
        path,
        branches: Vec::new(),
        error: None,
    };
    if let Err(e) = scan_into(&mut repo, worktrees, fetch) {
        repo.error = Some(e.to_string());
    }
    repo
}

fn scan_into(repo: &mut Repo, worktrees: &[PathBuf], fetch: bool) -> Result<()> {
    let dir = repo.path.clone();
    let mut branches = watched_branches(&dir, worktrees);
    if branches.is_empty() {
        anyhow::bail!("no remote branch to follow");
    }
    for b in &mut branches {
        b.old = rev(&dir, &b.remote_ref);
    }
    if fetch {
        let mut remotes: Vec<&str> = branches
            .iter()
            .filter_map(|b| b.remote_short().split_once('/').map(|(r, _)| r))
            .collect();
        remotes.sort();
        remotes.dedup();
        let errors: Vec<String> = remotes
            .iter()
            .filter_map(|r| git::fetch(&dir, r).err().map(|e| e.to_string()))
            .collect();
        if !errors.is_empty() {
            repo.error = Some(errors.join("; "));
        }
    }
    let me = query(&dir, &["config", "user.email"]).map(|e| e.to_lowercase());
    for b in &mut branches {
        b.new = rev(&dir, &b.remote_ref);
    }
    let default = branches.iter().find(|b| b.is_default);
    let default_name = default.map(|b| b.name.clone());
    let default_new = default.and_then(|b| b.new.clone());
    for b in branches.iter_mut().filter(|b| !b.is_default) {
        let Some((remote, name)) = b.remote_short().split_once('/') else {
            continue;
        };
        b.base = github::pr_base(&dir, remote, name)
            .filter(|base| Some(base) != default_name.as_ref())
            .map(|base| format!("{remote}/{base}"));
    }
    for b in &mut branches {
        let Some(new) = b.new.clone().filter(|new| b.old.as_ref() != Some(new)) else {
            continue;
        };
        if let Some(old) = &b.old {
            b.rewritten = !git::is_ancestor(&dir, old, &new);
            b.exclude.push(old.clone());
        }
        if !b.is_default {
            let base = b.base.as_ref().and_then(|base| rev(&dir, &format!("refs/remotes/{base}")));
            b.exclude.extend(base.or_else(|| default_new.clone()));
        }
        let mut revs = vec![new];
        revs.extend(b.exclude.iter().map(|e| format!("^{e}")));
        let revs: Vec<&str> = revs.iter().map(String::as_str).collect();
        b.commits = log(&dir, &revs, me.as_deref())?;
    }
    refresh_local(&dir, &mut branches);
    repo.branches = branches;
    Ok(())
}

/// The default branch, then the branches checked out in `worktrees`.
fn watched_branches(dir: &Path, worktrees: &[PathBuf]) -> Vec<Branch> {
    let mut refs: Vec<(String, String, bool)> = Vec::new();
    let remotes = query(dir, &["remote"]).unwrap_or_default();
    let remote = remotes
        .lines()
        .find(|r| *r == "origin")
        .or_else(|| remotes.lines().next());
    if let Some(remote) = remote {
        let head = query(dir, &["symbolic-ref", "--quiet", &format!("refs/remotes/{remote}/HEAD")]);
        let candidates = head
            .into_iter()
            .chain(["main", "master"].map(|b| format!("refs/remotes/{remote}/{b}")));
        for r in candidates {
            if rev(dir, &r).is_some() {
                let name = r.rsplit_once(&format!("{remote}/")).unwrap().1.to_string();
                refs.push((name, r, true));
                break;
            }
        }
    }
    for wt in worktrees {
        let Some(name) = query(wt, &["symbolic-ref", "--quiet", "--short", "HEAD"]) else {
            continue;
        };
        let upstream = query(wt, &["rev-parse", "--symbolic-full-name", &format!("{name}@{{upstream}}")]);
        if let Some(upstream) = upstream.filter(|u| u.starts_with("refs/remotes/")) {
            if !refs.iter().any(|(_, r, _)| *r == upstream) {
                refs.push((name, upstream, false));
            }
        }
    }
    refs.into_iter()
        .map(|(name, remote_ref, is_default)| Branch {
            name,
            remote_ref,
            is_default,
            old: None,
            new: None,
            base: None,
            exclude: Vec::new(),
            commits: Vec::new(),
            rewritten: false,
            local: None,
            note: None,
        })
        .collect()
}

fn log(dir: &Path, revs: &[&str], me: Option<&str>) -> Result<Vec<Commit>> {
    let mut args = vec!["log", "--max-count=1000", "--format=%h%x1f%an%x1f%ae%x1f%ce%x1f%ar%x1f%s"];
    args.extend(revs);
    args.push("--");
    Ok(git(dir, &args)?
        .lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(6, '\x1f').collect();
            let [short, author, author_email, committer_email, when, subject] = f[..] else {
                return None;
            };
            let is_me = |email: &str| me.is_some_and(|me| email.eq_ignore_ascii_case(me));
            let mine = match (is_me(author_email), is_me(committer_email)) {
                (false, _) => Mine::No,
                (true, true) => Mine::Pushed,
                (true, false) => Mine::Landed,
            };
            Some(Commit {
                short: short.into(),
                author: author.into(),
                when: when.into(),
                subject: subject.into(),
                mine,
            })
        })
        .collect())
}

/// Branch name to the worktree it is checked out in.
fn checkouts(dir: &Path) -> HashMap<String, PathBuf> {
    let mut result = HashMap::new();
    let mut path = None;
    for line in query(dir, &["worktree", "list", "--porcelain"]).unwrap_or_default().lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let (Some(b), Some(p)) = (line.strip_prefix("branch refs/heads/"), &path) {
            result.insert(b.to_string(), p.clone());
        }
    }
    result
}

pub fn refresh_local(dir: &Path, branches: &mut [Branch]) {
    let checkouts = checkouts(dir);
    for b in branches {
        let local_ref = format!("refs/heads/{}", b.name);
        b.local = rev(dir, &local_ref).map(|sha| {
            let counts = query(
                dir,
                &["rev-list", "--left-right", "--count", &format!("{local_ref}...{}", b.remote_ref)],
            )
            .unwrap_or_default();
            let mut counts = counts.split_whitespace().map(|n| n.parse().unwrap_or(0));
            Local {
                sha,
                checked_out_in: checkouts.get(&b.name).cloned(),
                ahead: counts.next().unwrap_or(0),
                behind: counts.next().unwrap_or(0),
            }
        });
    }
}

/// Fast-forwards the local branch to its remote-tracking branch, never merging.
pub fn pull(dir: &Path, b: &Branch) -> Result<String> {
    let Some(local) = &b.local else {
        return Ok("no local branch".into());
    };
    let Some(new) = &b.new else {
        return Ok("nothing to pull".into());
    };
    if local.behind == 0 {
        return Ok("up to date".into());
    }
    if local.ahead > 0 {
        anyhow::bail!("diverged ({} local commits), not pulling", local.ahead);
    }
    match &local.checked_out_in {
        // Merging in the worktree also refuses to overwrite uncommitted changes.
        Some(wt) => git(wt, &["merge", "--ff-only", "--quiet", new])?,
        None => git(
            dir,
            &["update-ref", "-m", "catchup: fast-forward", &format!("refs/heads/{}", b.name), new, &local.sha],
        )?,
    };
    Ok(format!("pulled {}", local.behind))
}
