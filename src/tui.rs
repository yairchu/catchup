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
use std::sync::mpsc::Receiver;
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
    /// Which entry each line of the list shows, as last drawn.
    line_keys: Vec<Option<Key>>,
}

enum Action {
    None,
    Quit,
    /// Run a program in the foreground, suspending the TUI.
    Foreground(Command),
}

pub fn run(targets: Vec<Target>, rx: Receiver<(usize, Repo)>) -> Result<()> {
    let mut app = App {
        repos: targets.iter().map(|_| None).collect(),
        targets,
        selected: None,
        status: String::new(),
        scroll: 0,
        list_area: Rect::default(),
        line_keys: Vec::new(),
    };
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
    fn event_loop(&mut self, terminal: &mut DefaultTerminal, rx: &Receiver<(usize, Repo)>) -> Result<()> {
        loop {
            while let Ok((i, repo)) = rx.try_recv() {
                self.repos[i] = Some(repo);
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
                        self.status = format!("could not run glog: {e}");
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
            _ => {}
        }
        Action::None
    }

    fn on_click(&mut self, column: u16, row: u16) -> Action {
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
        let mut program = vec!["glog", "log"];
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
            Ok(()) => format!("glog log {}", range.join(" ")),
            Err(e) => e.to_string(),
        };
        Action::None
    }

    fn pull(&mut self, (r, b): Key) -> Option<Result<String>> {
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
        self.lines().into_iter().filter_map(|(_, k)| k).collect()
    }

    fn lines(&self) -> Vec<(Line<'static>, Option<Key>)> {
        let mut lines = Vec::new();
        let mut pending = Vec::new();
        let mut quiet = Vec::new();
        for (r, target) in self.targets.iter().enumerate() {
            let Some(repo) = &self.repos[r] else {
                pending.push(scan::display_path(&target.worktrees[0]));
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
            lines.push((format!("fetching: {}", pending.join(", ")).fg(MUTED).into(), None));
        }
        if !quiet.is_empty() {
            lines.push((Line::from(""), None));
            lines.push((format!("nothing new: {}", quiet.join(", ")).fg(MUTED).into(), None));
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

        let lines = self.lines();
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
            text.push("Force-pushed: these are the commits not in the previous tip.".red().into());
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
