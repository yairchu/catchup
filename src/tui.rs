use crate::cmux;
use crate::scan::{self, Branch, Mine, Repo};
use crate::Target;
use anyhow::Result;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

const MUTED: Color = Color::Rgb(160, 160, 160);

type Key = (usize, usize);

struct App {
    targets: Vec<Target>,
    repos: Vec<Option<Repo>>,
    selected: Option<Key>,
    status: String,
    scroll: usize,
    list_area: Rect,
    refresh_area: Rect,
    fetch: bool,
    tx: Sender<(usize, Repo)>,
    pending: Vec<bool>,
    refreshing: bool,
    /// Which entry each line of the list shows, as last drawn.
    line_keys: Vec<Option<Key>>,
}

enum Action {
    None,
    Quit,
    /// Run a program in the foreground, suspending the TUI.
    Foreground(Command),
}

pub fn run(
    targets: Vec<Target>,
    fetch: bool,
    tx: Sender<(usize, Repo)>,
    rx: Receiver<(usize, Repo)>,
) -> Result<()> {
    let mut app = App::new(targets, fetch, tx);
    let mut terminal = start();
    let result = app.event_loop(&mut terminal, &rx);
    stop();
    result
}

fn start() -> DefaultTerminal {
    let terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    terminal
}

fn stop() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
}

impl App {
    fn new(targets: Vec<Target>, fetch: bool, tx: Sender<(usize, Repo)>) -> Self {
        Self {
            repos: targets.iter().map(|_| None).collect(),
            pending: targets.iter().map(|_| true).collect(),
            targets,
            fetch,
            tx,
            refreshing: false,
            selected: None,
            status: String::new(),
            scroll: 0,
            list_area: Rect::default(),
            refresh_area: Rect::default(),
            line_keys: Vec::new(),
        }
    }

    fn refresh(&mut self) {
        if self.pending.iter().any(|pending| *pending) {
            self.status = "refresh already in progress".into();
            return;
        }
        self.refreshing = true;
        self.status = if self.fetch {
            "refreshing…"
        } else {
            "refreshing (no fetch)…"
        }
        .into();
        for (i, target) in self.targets.iter().enumerate() {
            self.pending[i] = true;
            let repo = self.repos[i].clone();
            let worktrees = target.worktrees.clone();
            let tx = self.tx.clone();
            let fetch = self.fetch;
            std::thread::spawn(move || {
                let repo = match repo {
                    Some(repo) => scan::refresh(repo, &worktrees, fetch),
                    None => scan::scan(&worktrees, fetch),
                };
                let _ = tx.send((i, repo));
            });
        }
    }

    fn receive(&mut self, i: usize, repo: Repo) {
        // Branch order can change when a worktree switches branches.
        if let Some((r, b)) = self.selected.filter(|(r, _)| *r == i) {
            self.selected = self.repos[r]
                .as_ref()
                .and_then(|old| old.branches.get(b))
                .and_then(|old| {
                    repo.branches.iter().position(|branch| {
                        branch.name == old.name && branch.remote_ref == old.remote_ref
                    })
                })
                .map(|b| (r, b));
        }
        self.repos[i] = Some(repo);
        self.pending[i] = false;
        if self.refreshing && !self.pending.iter().any(|pending| *pending) {
            self.refreshing = false;
            let failed = self
                .repos
                .iter()
                .flatten()
                .filter(|repo| repo.error.is_some())
                .count();
            self.status = if failed == 0 {
                "refreshed".into()
            } else {
                format!("refreshed; {failed} with errors")
            };
        }
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal, rx: &Receiver<(usize, Repo)>) -> Result<()> {
        loop {
            while let Ok((i, repo)) = rx.try_recv() {
                self.receive(i, repo);
            }
            if self.selected.is_none_or(|k| !self.entries().contains(&k)) {
                self.selected = self.entries().first().copied();
            }
            terminal.draw(|f| self.draw(f))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let action = match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                        Action::Quit
                    } else {
                        self.on_key(k.code)
                    }
                }
                Event::Mouse(m) => match m.kind {
                    MouseEventKind::Down(MouseButton::Left) => self.on_click(m.column, m.row),
                    MouseEventKind::ScrollDown => self.on_key(KeyCode::Down),
                    MouseEventKind::ScrollUp => self.on_key(KeyCode::Up),
                    _ => Action::None,
                },
                _ => Action::None,
            };
            match action {
                Action::None => {}
                Action::Quit => return Ok(()),
                Action::Foreground(mut cmd) => {
                    stop();
                    if let Err(e) = cmd.status() {
                        self.status = format!("could not run {:?}: {e}", cmd.get_program());
                    }
                    *terminal = start();
                }
            }
        }
    }

    fn on_key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Enter | KeyCode::Char('o') => return self.open(false),
            KeyCode::Char('w') => return self.open(true),
            KeyCode::Char('p') => self.pull_selected(),
            KeyCode::Char('P') => self.pull_all(),
            KeyCode::Char('r') => self.refresh(),
            _ => {}
        }
        Action::None
    }

    fn on_click(&mut self, column: u16, row: u16) -> Action {
        if self.refresh_area.contains((column, row).into()) {
            self.refresh();
            return Action::None;
        }
        let a = self.list_area;
        if column < a.x || column >= a.x + a.width || row < a.y || row >= a.y + a.height {
            return Action::None;
        }
        let line = (row - a.y) as usize + self.scroll;
        match self.line_keys.get(line).copied().flatten() {
            Some(key) if Some(key) == self.selected => self.open(false),
            Some(key) => {
                self.selected = Some(key);
                Action::None
            }
            None => Action::None,
        }
    }

    fn step(&mut self, delta: isize) {
        let entries = self.entries();
        let Some(pos) = self.selected.and_then(|k| entries.iter().position(|e| *e == k)) else {
            return;
        };
        let pos = pos.saturating_add_signed(delta).min(entries.len() - 1);
        self.selected = Some(entries[pos]);
    }

    fn branch(&self, (r, b): Key) -> Option<(&Repo, &Branch)> {
        let repo = self.repos[r].as_ref()?;
        Some((repo, repo.branches.get(b)?))
    }

    fn open(&mut self, in_workspace: bool) -> Action {
        let Some((repo, branch)) = self.selected.and_then(|k| self.branch(k)) else {
            return Action::None;
        };
        let Some(range) = branch.log_range() else {
            self.status = "nothing to show".into();
            return Action::None;
        };
        let mut program = match on_path("glog") {
            true => vec!["glog", "log", "--exit-on-esc"],
            false => vec!["git", "log", "-p"],
        };
        program.extend(range.iter().map(String::as_str));
        if !in_workspace {
            let mut cmd = Command::new(program[0]);
            cmd.args(&program[1..]).current_dir(&repo.path);
            return Action::Foreground(cmd);
        }
        let command = cmux::shell_command(&repo.path, &program);
        let workspace = self.targets[self.selected.unwrap().0].workspace.clone();
        let Some(ws) = workspace.filter(|_| cmux::inside_cmux()) else {
            self.status = "this repo is not in a cmux workspace".into();
            return Action::None;
        };
        let result =
            cmux::new_split(Some(&ws.id), &command).and_then(|_| cmux::select_workspace(&ws.id));
        self.status = match result {
            Ok(()) => program.join(" "),
            Err(e) => e.to_string(),
        };
        Action::None
    }

    fn pull(&mut self, (r, b): Key) -> Option<Result<String>> {
        if self.pending[r] {
            return Some(Err(anyhow::anyhow!("repo is refreshing; wait before pulling")));
        }
        let repo = self.repos[r].as_mut()?;
        let result = scan::pull(&repo.path, repo.branches.get(b)?);
        repo.branches[b].note = Some(match &result {
            Ok(msg) => msg.clone(),
            Err(e) => e.to_string(),
        });
        scan::refresh_local(&repo.path.clone(), &mut repo.branches);
        Some(result)
    }

    fn pull_selected(&mut self) {
        if let Some(key) = self.selected {
            self.status = match self.pull(key) {
                Some(Ok(msg)) => msg,
                Some(Err(e)) => e.to_string(),
                None => String::new(),
            };
        }
    }

    fn pull_all(&mut self) {
        if self.pending.iter().any(|pending| *pending) {
            self.status = "wait for fetching to finish before pulling all".into();
            return;
        }
        let behind: Vec<(Key, usize)> = self
            .entries()
            .into_iter()
            .filter_map(|k| {
                let local = self.branch(k)?.1.local.as_ref()?;
                (local.behind > 0).then_some((k, local.ahead))
            })
            .collect();
        let (mut pulled, mut failed, mut diverged) = (0, 0, 0);
        for (key, ahead) in behind {
            if ahead > 0 {
                diverged += 1;
                continue;
            }
            match self.pull(key) {
                Some(Ok(_)) => pulled += 1,
                _ => failed += 1,
            }
        }
        self.status = format!("pulled {pulled}");
        if diverged > 0 {
            self.status += &format!(", {diverged} diverged");
        }
        if failed > 0 {
            self.status += &format!(", {failed} failed");
        }
    }

    /// Selectable entries, in display order.
    fn entries(&self) -> Vec<Key> {
        self.lines(self.list_area.width).into_iter().filter_map(|(_, k)| k).collect()
    }

    fn lines(&self, width: u16) -> Vec<(Line<'static>, Option<Key>)> {
        let mut lines = Vec::new();
        let mut pending = Vec::new();
        let mut quiet = Vec::new();
        for (r, target) in self.targets.iter().enumerate() {
            if self.pending[r] {
                pending.push(scan::display_path(&target.worktrees[0]));
            }
            let Some(repo) = &self.repos[r] else {
                continue;
            };
            let shown: Vec<usize> = (0..repo.branches.len())
                .filter(|&b| repo.branches[b].interesting())
                .collect();
            if shown.is_empty() && repo.error.is_none() {
                quiet.push(repo.display.clone());
                continue;
            }
            let mut header = vec![Span::from(repo.display.clone()).bold()];
            if let Some(ws) = &target.workspace {
                header.push(format!("  {}", ws.name).fg(MUTED));
            }
            if let Some(e) = &repo.error {
                header.push(format!("  {}", first_line(e)).red());
            }
            lines.push((Line::from(header), None));
            for b in shown {
                let branch = &repo.branches[b];
                let checked_out = if branch.local.as_ref().is_some_and(|l| l.checked_out_in.is_some()) {
                    "* "
                } else {
                    "  "
                };
                let mut spans = vec![Span::from(format!("  {checked_out}{:<18} ", branch.name))];
                spans.extend(summary(branch));
                lines.push((Line::from(spans), Some((r, b))));
            }
        }
        if !pending.is_empty() {
            lines.push((Line::from(""), None));
            let verb = if self.fetch { "fetching" } else { "scanning" };
            lines.push((format!("{verb}: {}", pending.join(", ")).fg(MUTED).into(), None));
        }
        if !quiet.is_empty() {
            lines.push((Line::from(""), None));
            let summary = format!("nothing new: {}", quiet.join(", "));
            lines.extend(
                textwrap::wrap(&summary, usize::from(width.max(1)))
                    .into_iter()
                    .map(|line| (line.into_owned().fg(MUTED).into(), None)),
            );
        }
        lines
    }

    fn draw(&mut self, f: &mut Frame) {
        let [list, detail, footer] = Layout::vertical([
            Constraint::Min(5),
            Constraint::Percentage(45),
            Constraint::Length(1),
        ])
        .areas(f.area());
        self.list_area = list;

        let lines = self.lines(list.width);
        let selected_line = lines.iter().position(|(_, k)| k.is_some() && *k == self.selected);
        if let Some(sel) = selected_line {
            let height = list.height as usize;
            if sel < self.scroll + 1 {
                // Keep the repo header above the selection in view.
                self.scroll = sel.saturating_sub(1);
            } else if sel >= self.scroll + height {
                self.scroll = sel + 1 - height;
            }
        }
        self.line_keys = lines.iter().map(|(_, k)| *k).collect();
        let text: Vec<Line> = lines
            .into_iter()
            .enumerate()
            .map(|(i, (line, _))| {
                if Some(i) == selected_line {
                    line.patch_style(Style::new().add_modifier(Modifier::REVERSED))
                } else {
                    line
                }
            })
            .collect();
        let empty = text.is_empty();
        f.render_widget(Paragraph::new(text).scroll((self.scroll as u16, 0)), list);
        if empty {
            f.render_widget(Paragraph::new("fetching…".fg(MUTED)), list);
        }

        self.draw_detail(f, detail);

        let [refresh, footer] = Layout::horizontal([Constraint::Length(11), Constraint::Min(0)]).areas(footer);
        self.refresh_area = refresh;
        let style = if self.pending.iter().any(|pending| *pending) {
            Style::new().fg(MUTED)
        } else {
            Style::new().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED)
        };
        f.render_widget(Paragraph::new(Span::styled("r Refresh", style)), refresh);
        let keys = "↑↓ select  ⏎ log  w log in its workspace  p pull  P pull all  q quit";
        let footer_text = if self.status.is_empty() {
            Line::from(keys.fg(MUTED))
        } else {
            Line::from(vec![Span::from(self.status.clone()).yellow(), "   ".into(), keys.fg(MUTED)])
        };
        f.render_widget(Paragraph::new(footer_text), footer);
    }

    fn draw_detail(&self, f: &mut Frame, area: Rect) {
        let Some((repo, branch)) = self.selected.and_then(|k| self.branch(k)) else {
            f.render_widget(Block::default().borders(Borders::TOP), area);
            return;
        };
        let title = format!(" {} · {} ", repo.display, branch.remote_short());
        let block = Block::default().borders(Borders::TOP).title(title);
        let mut text: Vec<Line> = Vec::new();
        if let Some(note) = &branch.note {
            text.push(note.clone().yellow().into());
        }
        if branch.rewritten {
            text.push("Force-pushed: showing commits new since catchup opened.".red().into());
        }
        if branch.commits.is_empty() {
            if let Some(l) = branch.local.as_ref().filter(|l| l.behind > 0) {
                text.push(
                    format!("Nothing new in this fetch; {} commits fetched earlier are not pulled yet.", l.behind)
                        .fg(MUTED)
                        .into(),
                );
            }
        }
        text.extend(branch.commits.iter().map(commit_line));
        f.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), area);
    }
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

fn commit_line(c: &scan::Commit) -> Line<'static> {
    let (marker, style) = match c.mine {
        Mine::No => ("  ", Style::new()),
        Mine::Pushed => ("  ", Style::new().fg(MUTED)),
        Mine::Landed => ("★ ", Style::new().fg(Color::Magenta)),
    };
    let mut spans = vec![
        Span::styled(marker, style),
        Span::styled(format!("{} ", c.short), style.fg(if c.mine == Mine::Pushed { MUTED } else { Color::Yellow })),
        Span::styled(format!("{:<16} ", c.author), style.fg(if c.mine == Mine::Pushed { MUTED } else { Color::Cyan })),
        Span::styled(c.subject.clone(), style),
    ];
    spans.push(Span::styled(format!("  {}", c.when), Style::new().fg(MUTED)));
    Line::from(spans)
}

/// One-line description of what happened to a branch.
fn summary(b: &Branch) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let sep = |spans: &mut Vec<Span<'static>>| {
        if !spans.is_empty() {
            spans.push("  ".into());
        }
    };
    let others: Vec<&scan::Commit> = b.commits.iter().filter(|c| c.mine != Mine::Pushed).collect();
    let landed = b.commits.iter().filter(|c| c.mine == Mine::Landed).count();
    let pushed = b.commits.len() - others.len();
    if b.rewritten {
        spans.push("force-pushed".red().bold());
    }
    if !others.is_empty() {
        sep(&mut spans);
        spans.push(format!("+{}", others.len()).green().bold());
        spans.push(format!(" by {}", authors(&others)).into());
    }
    if landed > 0 {
        sep(&mut spans);
        spans.push(format!("★ {landed} of yours landed").magenta());
    }
    if pushed > 0 {
        sep(&mut spans);
        spans.push(format!("+{pushed} yours").fg(MUTED));
    }
    match &b.local {
        Some(l) if l.behind > 0 && l.ahead > 0 => {
            sep(&mut spans);
            spans.push(format!("diverged ↑{} ↓{}", l.ahead, l.behind).red());
        }
        Some(l) if l.behind > 0 => {
            sep(&mut spans);
            spans.push(format!("↓{} to pull", l.behind).cyan());
        }
        Some(_) => {}
        None => {
            sep(&mut spans);
            spans.push("no local branch".fg(MUTED));
        }
    }
    if let Some(base) = &b.base {
        sep(&mut spans);
        spans.push(format!("onto {base}").fg(MUTED));
    }
    if let Some(note) = &b.note {
        sep(&mut spans);
        spans.push(note.clone().yellow());
    }
    spans
}

/// Authors by number of commits, most first.
fn authors(commits: &[&scan::Commit]) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for c in commits {
        *counts.entry(c.author.as_str()).or_default() += 1;
    }
    let mut names: Vec<(&str, usize)> = counts.into_iter().collect();
    names.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let shown: Vec<&str> = names.iter().take(3).map(|(n, _)| *n).collect();
    match names.len() {
        n if n > 3 => format!("{} +{} more", shown.join(", "), n - 3),
        _ => shown.join(", "),
    }
}

fn plain(spans: &[Span]) -> String {
    spans.iter().map(|s| s.content.as_ref()).collect()
}

pub fn print(repos: &[Repo]) {
    let mut quiet = Vec::new();
    for repo in repos {
        let shown: Vec<&Branch> = repo.branches.iter().filter(|b| b.interesting()).collect();
        if shown.is_empty() && repo.error.is_none() {
            quiet.push(repo.display.as_str());
            continue;
        }
        println!("{}", repo.display);
        if let Some(e) = &repo.error {
            println!("  error: {}", first_line(e));
        }
        for b in shown {
            println!("  {:<18} {}", b.name, plain(&summary(b)));
            for c in &b.commits {
                println!("    {}", plain(&commit_line(c).spans));
            }
        }
    }
    if !quiet.is_empty() {
        println!("nothing new: {}", quiet.join(", "));
    }
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use std::{path::PathBuf, sync::mpsc};

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.into(),
            remote_ref: format!("refs/remotes/origin/{name}"),
            is_default: name == "main",
            old: None,
            new: None,
            base: None,
            exclude: Vec::new(),
            commits: Vec::new(),
            rewritten: true,
            local: None,
            note: None,
        }
    }

    fn repo(names: &[&str]) -> Repo {
        Repo {
            // A nonexistent path makes the refresh worker return a scan error
            // without contacting a remote or changing a real repository.
            path: PathBuf::from("/dev/null/catchup-test"),
            display: "test repo".into(),
            branches: names.iter().map(|name| branch(name)).collect(),
            error: None,
        }
    }

    fn app() -> (App, Receiver<(usize, Repo)>) {
        let (tx, rx) = mpsc::channel();
        let target = Target {
            worktrees: vec![repo(&[]).path],
            workspace: None,
        };
        (App::new(vec![target], true, tx), rx)
    }

    #[test]
    fn quiet_summary_wraps_and_reflows_on_resize() {
        let (mut app, _) = app();
        let mut quiet = repo(&[]);
        quiet.display = "~/one, ~/two, ~/three".into();
        app.receive(0, quiet);

        let mut terminal = Terminal::new(TestBackend::new(20, 20)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let row = |terminal: &Terminal<TestBackend>, y| {
            (0..terminal.size().unwrap().width)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        };
        assert_eq!(row(&terminal, 1), "nothing new: ~/one,");
        assert_eq!(row(&terminal, 2), "~/two, ~/three");
        assert!(app.line_keys.iter().all(Option::is_none));

        terminal.backend_mut().resize(40, 20);
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(row(&terminal, 1), "nothing new: ~/one, ~/two, ~/three");
        assert_eq!(row(&terminal, 2), "");
    }

    #[test]
    fn refresh_keeps_selected_branch_when_branch_order_changes() {
        let (mut app, _) = app();
        app.receive(0, repo(&["main", "feature"]));
        app.selected = Some((0, 1));
        app.receive(0, repo(&["feature", "main"]));
        assert_eq!(app.selected, Some((0, 0)));
        assert_eq!(app.branch(app.selected.unwrap()).unwrap().1.name, "feature");
        app.receive(0, repo(&["main"]));
        assert!(app.selected.is_none());
    }

    #[test]
    fn footer_click_refreshes_in_background_and_prevents_overlapping_operations() {
        let (mut app, rx) = app();
        app.on_key(KeyCode::Char('r'));
        assert!(rx.try_recv().is_err(), "wait for the initial scan");
        app.receive(0, repo(&["main"]));
        app.selected = Some((0, 0));

        let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(terminal.backend().buffer()[(0, 24)].symbol(), "r");
        let button = app.refresh_area;
        app.on_click(button.x, button.y);
        assert!(app.refreshing);
        assert!(app.pending[0]);
        assert_eq!(app.entries(), vec![(0, 0)], "keep existing entries usable");
        app.on_key(KeyCode::Char('r'));
        assert_eq!(app.status, "refresh already in progress");
        assert!(
            app.pull((0, 0)).unwrap().is_err(),
            "wait before pulling a refreshing repo"
        );
        let (i, repo) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        app.receive(i, repo);
        assert!(!app.refreshing);
        assert!(!app.pending[0]);
        assert_eq!(app.selected, Some((0, 0)));
        assert_eq!(app.status, "refreshed; 1 with errors");
        assert!(
            rx.try_recv().is_err(),
            "repeated refresh must not spawn another worker"
        );
    }
}
