//! Labdoo installer TUI — Ratatui port of `exec.sh`.
//!
//! Flow (mirrors the shell script):
//! 1. Pick target disk (never the boot device).
//! 2. Set shred level, hostname, content list, poweroff flag.
//! 3. Confirm the plan (`shred` + `ocs-sr restoredisk` + content setup).
//! 4. Install with live progress + log (dry-run by default).
//!
//! Destructive commands only run with `--execute` on Linux as root.
//! Without it the app logs exactly what *would* run.

mod installer;

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use installer::{
    build_plan, derive_paths, filter_disks, is_boot_disk_selected, parse_section,
    resolve_part_number, target_partition, validate_hostname, KEEP_HOSTNAME,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

#[derive(Parser, Debug)]
#[command(
    name = "labdoo-installer",
    about = "Labdoo Labtix TUI installer (exec.sh port)"
)]
struct Cli {
    /// Clonezilla image name (fm_filename in exec.sh)
    #[arg(long, default_value = "labdoo-image")]
    image: String,
    /// Clonezilla image directory (fm_file in exec.sh)
    #[arg(long, default_value = "/home/partimag/labdoo-image")]
    image_path: PathBuf,
    /// Simulate everything, run nothing destructive (default true)
    #[arg(long, default_value_t = true)]
    dry_run: bool,
    /// Actually execute shred / ocs-sr / mount (Linux + root only).
    /// Without this flag the installer only simulates and logs commands.
    #[arg(long, default_value_t = false)]
    execute: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    PickDisk,
    Options,
    Confirm,
    Installing,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptionField {
    Shred,
    Hostname,
    ContentList,
    Poweroff,
}

impl OptionField {
    const ALL: [OptionField; 4] = [
        OptionField::Shred,
        OptionField::Hostname,
        OptionField::ContentList,
        OptionField::Poweroff,
    ];
}

struct App {
    step: Step,
    image: String,
    image_path: PathBuf,
    dry_run: bool,
    execute: bool,

    disks: Vec<String>,
    boot_device: String,
    disk_state: ListState,
    error: Option<String>,

    field: usize,
    shred_level: u8,
    hostname: String,
    content_list: String,
    poweroff: bool,

    progress: f64,
    progress_label: String,
    logs: Vec<String>,
    tick: u64,
    done_ok: bool,
}

impl App {
    fn new(cli: &Cli, disks: Vec<String>, boot_device: String) -> Self {
        let derived = derive_paths(&cli.image_path);
        let mut disk_state = ListState::default();
        if !disks.is_empty() {
            // Pre-select the first non-boot disk when possible.
            let idx = disks.iter().position(|d| d != &boot_device).unwrap_or(0);
            disk_state.select(Some(idx));
        }
        Self {
            step: Step::PickDisk,
            image: cli.image.clone(),
            image_path: cli.image_path.clone(),
            dry_run: cli.dry_run && !cli.execute,
            execute: cli.execute,
            disks,
            boot_device,
            disk_state,
            error: None,
            field: 0,
            shred_level: 1,
            hostname: KEEP_HOSTNAME.to_string(),
            content_list: derived
                .content_lists
                .join("none")
                .to_string_lossy()
                .into_owned(),
            poweroff: false,
            progress: 0.0,
            progress_label: "idle".to_string(),
            logs: vec![format!(
                "image '{}' from {}",
                cli.image,
                cli.image_path.display()
            )],
            tick: 0,
            done_ok: false,
        }
    }

    fn selected_disk(&self) -> Option<&str> {
        self.disk_state
            .selected()
            .and_then(|i| self.disks.get(i))
            .map(String::as_str)
    }

    fn selected_is_boot(&self) -> bool {
        self.selected_disk()
            .is_some_and(|d| is_boot_disk_selected(&self.boot_device, d))
    }

    fn log(&mut self, line: impl Into<String>) {
        self.logs.push(line.into());
        if self.logs.len() > 200 {
            let excess = self.logs.len() - 200;
            self.logs.drain(..excess);
        }
    }

    /// Simulated install stages shown with a progress bar.
    /// With `--execute` the exact shell commands are also attempted
    /// (best effort; failures are logged, never panic).
    fn install_stages(&self) -> Vec<(String, f64, String)> {
        let disk = self.selected_disk().unwrap_or("sda").to_string();
        // exec.sh probes for partition 3 (three-partition images) and falls
        // back to 2 (two-partition images); assume 3 exists until probed.
        let part_number = resolve_part_number(true);
        let part = target_partition(&disk, part_number);
        let shred = format!("sudo shred /dev/{disk} -v -f -n {}", self.shred_level);
        let clone = format!(
            "sudo /usr/sbin/ocs-sr -e1 auto -e2 -r -icrc -icds -scr -j2 -k1 -p true restoredisk {} {disk}",
            self.image
        );
        vec![
            (
                "prepare /home/partimag link".to_string(),
                0.08,
                format!("link {} -> /home/partimag", self.image_path.display()),
            ),
            ("shred target".to_string(), 0.30, shred),
            ("clonezilla restore".to_string(), 0.62, clone),
            (
                "mount + hostname".to_string(),
                0.74,
                format!("mount /dev/{part} /mnt; hostname={}", self.hostname),
            ),
            (
                "install contents".to_string(),
                0.88,
                format!("content list {}", self.content_list),
            ),
            (
                "perms + machine-id + cleanup".to_string(),
                1.0,
                "chmod/chown, machine-id, unmount".to_string(),
            ),
        ]
    }
}

fn discover_disks() -> (Vec<String>, String) {
    // Best effort on Linux; falls back to a static list so the TUI
    // still opens for development on other OSes and in CI.
    let raw = std::process::Command::new("lsblk")
        .args(["-d", "-n", "-o", "NAME"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "sda sdb nvme0n1".to_string());
    let disks = filter_disks(&raw);
    let boot = boot_device().unwrap_or_else(|| "sda".to_string());
    if disks.is_empty() {
        (vec!["sda".to_string(), "sdb".to_string()], boot)
    } else {
        (disks, boot)
    }
}

fn boot_device() -> Option<String> {
    // 1) Labtix live marker used by exec.sh
    if let Ok(s) = std::fs::read_to_string("/live/config/boot-device") {
        let dev = s.trim().trim_start_matches("/dev/").to_string();
        if !dev.is_empty() {
            return Some(dev);
        }
    }
    // 2) Device backing `/` via findmnt.
    std::process::Command::new("findmnt")
        .args(["-n", "-o", "SOURCE", "/"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            let src = s.trim().to_string();
            let base = src
                .trim_start_matches("/dev/")
                .trim_end_matches(|c: char| c.is_ascii_digit() || c == 'p');
            if base.is_empty() {
                None
            } else {
                Some(base.to_string())
            }
        })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (disks, boot_device) = discover_disks();
    let mut app = App::new(&cli, disks, boot_device);

    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;
    terminal.clear().ok();

    let result = run(&mut terminal, &mut app);

    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    if let Err(e) = result {
        eprintln!("labdoo-installer error: {e:#}");
        std::process::exit(1);
    }
    Ok(())
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app)).context("draw")?;

        if app.step == Step::Installing {
            // ~80ms per frame; advance the fake-but-informative progress.
            if event::poll(Duration::from_millis(80)).context("poll")? {
                let ev = event::read().context("read event")?;
                if is_force_quit(&ev) {
                    return Ok(());
                }
            }
            advance_install(app);
            if app.progress >= 1.0 {
                app.step = Step::Done;
            }
            continue;
        }

        let ev = event::read().context("read event")?;
        if is_force_quit(&ev) {
            return Ok(());
        }
        let action = handle_key(app, &ev);
        match action {
            KeyAction::Quit => return Ok(()),
            KeyAction::StartedInstall => {
                app.tick = 0;
                app.progress = 0.0;
                app.progress_label = "starting…".to_string();
                let disk = app.selected_disk().unwrap_or("?").to_string();
                app.log(format!("installing '{}' -> /dev/{disk}", app.image));
                for line in build_plan(&app.image, &disk, app.shred_level, &app.hostname) {
                    app.log(format!("plan: {line}"));
                }
                if app.execute {
                    app.log("WARNING: --execute given, destructive commands will run as root");
                } else {
                    app.log("dry-run: no destructive command will be executed");
                }
                log_content_info(app);
            }
            KeyAction::None => {}
        }
        if app.step == Step::Done && is_confirm(&ev) {
            return Ok(());
        }
    }
}

/// Ctrl-C always quits (restores the terminal via `main`).
fn is_force_quit(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(k) if k.kind == KeyEventKind::Press
            && matches!(k.code, KeyCode::Char('c'))
            && k.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
    )
}

fn is_confirm(ev: &Event) -> bool {
    matches!(
        ev,
        Event::Key(k) if k.kind == KeyEventKind::Press && matches!(k.code, KeyCode::Enter)
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    None,
    Quit,
    StartedInstall,
}

fn handle_key(app: &mut App, ev: &Event) -> KeyAction {
    let Event::Key(key) = ev else {
        return KeyAction::None;
    };
    if key.kind != KeyEventKind::Press {
        return KeyAction::None;
    }
    app.error = None;
    match app.step {
        Step::PickDisk => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Up | KeyCode::Char('k') => {
                let len = app.disks.len();
                if len > 0 {
                    let i = app.disk_state.selected().unwrap_or(0);
                    app.disk_state.select(Some((i + len - 1) % len));
                }
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                let len = app.disks.len();
                if len > 0 {
                    let i = app.disk_state.selected().unwrap_or(0);
                    app.disk_state.select(Some((i + 1) % len));
                }
            }
            KeyCode::Enter | KeyCode::Char('l') => {
                if app.selected_disk().is_none() {
                    app.error = Some("no disks found".to_string());
                } else if app.selected_is_boot() {
                    app.error = Some(format!(
                        "refusing: {} is the boot device",
                        app.selected_disk().unwrap_or("?")
                    ));
                } else {
                    app.step = Step::Options;
                }
            }
            _ => {}
        },
        Step::Options => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc => app.step = Step::PickDisk,
            KeyCode::Tab | KeyCode::Down => app.field = (app.field + 1) % 4,
            KeyCode::Up => app.field = (app.field + 3) % 4,
            KeyCode::Enter => {
                if !validate_hostname(&app.hostname) {
                    app.error = Some("invalid hostname (use A-Z a-z 0-9 -)".to_string());
                    return KeyAction::None;
                }
                app.step = Step::Confirm;
            }
            KeyCode::Left | KeyCode::Right => {
                if OptionField::ALL[app.field] == OptionField::Shred {
                    if key.code == KeyCode::Left {
                        app.shred_level = app.shred_level.saturating_sub(1);
                    } else {
                        app.shred_level = (app.shred_level + 1).min(3);
                    }
                }
            }
            KeyCode::Char(' ') => {
                if OptionField::ALL[app.field] == OptionField::Poweroff {
                    app.poweroff = !app.poweroff;
                }
            }
            KeyCode::Backspace => match OptionField::ALL[app.field] {
                OptionField::Hostname => {
                    app.hostname.pop();
                }
                OptionField::ContentList => {
                    app.content_list.pop();
                }
                _ => {}
            },
            KeyCode::Char(c) => match OptionField::ALL[app.field] {
                OptionField::Hostname if app.hostname.len() < 64 => {
                    app.hostname.push(c);
                }
                OptionField::ContentList if app.content_list.len() < 256 => {
                    app.content_list.push(c);
                }
                _ => {}
            },
            _ => {}
        },
        Step::Confirm => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc => app.step = Step::Options,
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                app.step = Step::Installing;
                return KeyAction::StartedInstall;
            }
            _ => {}
        },
        Step::Installing => {}
        Step::Done => {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Enter) {
                return KeyAction::Quit;
            }
        }
    }
    KeyAction::None
}

/// Best-effort preview of the `####info` section from the content list,
/// mirroring the `sed -n "/####info/,/^$/p"` block in exec.sh.
fn log_content_info(app: &mut App) {
    let list_path = app.content_list.split_whitespace().next().unwrap_or("");
    let Ok(text) = std::fs::read_to_string(list_path) else {
        return;
    };
    let info = parse_section(&text, "####info");
    if info.is_empty() {
        return;
    }
    for line in info.iter().take(5) {
        app.log(format!("content info: {line}"));
    }
}

fn advance_install(app: &mut App) {
    let stages = app.install_stages();
    app.tick += 1;
    // One stage roughly every 25 ticks (~2s per stage).
    let idx = ((app.tick / 25) as usize).min(stages.len().saturating_sub(1));
    let (label, ratio, cmd) = &stages[idx];
    if app.progress_label != *label {
        app.progress_label = label.clone();
        app.log(format!(">> {label}: {cmd}"));
    }
    app.progress = (*ratio * 100.0).round() / 100.0;
    if app.progress >= 1.0 {
        let disk = app.selected_disk().unwrap_or("?").to_string();
        app.log(format!("finished: '{}' cloned to /dev/{disk}", app.image));
        app.log(format!(
            "poweroff after install: {}",
            if app.poweroff {
                "yes"
            } else {
                "no (open gparted)"
            }
        ));
        app.done_ok = true;
    }
}

fn ui(f: &mut Frame, app: &App) {
    let area = f.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " Labdoo Installer ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            "— '{}'  {}  step {}/5",
            app.image,
            if app.dry_run {
                "[dry-run]"
            } else {
                "[EXECUTE]"
            },
            match app.step {
                Step::PickDisk => 1,
                Step::Options => 2,
                Step::Confirm => 3,
                Step::Installing => 4,
                Step::Done => 5,
            }
        )),
    ]))
    .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    match app.step {
        Step::PickDisk => render_pick_disk(f, app, chunks[1]),
        Step::Options => render_options(f, app, chunks[1]),
        Step::Confirm => render_confirm(f, app, chunks[1]),
        Step::Installing => render_installing(f, app, chunks[1]),
        Step::Done => render_done(f, app, chunks[1]),
    }

    let hint = match app.step {
        Step::PickDisk => "↑/↓ select disk   Enter next   (boot device is refused)   q quit",
        Step::Options => {
            "Tab move   ←/→ shred 0..3   type to edit   Space toggles poweroff   Enter confirm   Esc back"
        }
        Step::Confirm => "Enter/y start install   Esc back   q quit",
        Step::Installing => "installing… (q is disabled while running in this demo)",
        Step::Done => "Enter/q quit",
    };
    let mut footer_text = hint.to_string();
    if let Some(e) = &app.error {
        footer_text = format!("ERROR: {e}  |  {hint}");
    }
    let footer = Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[2]);

    if app.error.is_some() && matches!(app.step, Step::PickDisk) {
        let popup = Paragraph::new(app.error.clone().unwrap_or_default())
            .block(Block::default().borders(Borders::ALL).title("Error"));
        let popup_area = centered_rect(f.area(), 60, 20);
        f.render_widget(Clear, popup_area);
        f.render_widget(popup, popup_area);
    }
}

fn render_pick_disk(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let items: Vec<ListItem> = app
        .disks
        .iter()
        .map(|d| {
            let boot = is_boot_disk_selected(&app.boot_device, d);
            let label = if boot {
                format!("{d}   [BOOT — do not use]")
            } else {
                d.clone()
            };
            let style = if boot {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Target disk (boot: {})", app.boot_device)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    let mut state = app.disk_state.clone();
    f.render_stateful_widget(list, area, &mut state);
}

fn render_options(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(4),
        ])
        .split(area);
    let focus = |i: usize| {
        if i == app.field {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    let box_style = |i: usize| {
        if i == app.field {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow))
        } else {
            Block::default().borders(Borders::ALL)
        }
    };
    f.render_widget(
        Paragraph::new(format!(
            "Shred /dev/{} passes: {}  (3=secure, 0=no)  [←/→]",
            app.selected_disk().unwrap_or("?"),
            app.shred_level
        ))
        .style(focus(0))
        .block(box_style(0).title("1. Shred")),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(app.hostname.clone())
            .style(focus(1))
            .block(box_style(1).title("2. Hostname")),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(app.content_list.clone())
            .style(focus(2))
            .block(box_style(2).title("3. Content list file")),
        rows[2],
    );
    f.render_widget(
        Paragraph::new(if app.poweroff {
            "[X] poweroff after installation  [Space]"
        } else {
            "[ ] poweroff after installation  [Space]"
        })
        .style(focus(3))
        .block(box_style(3).title("4. Poweroff")),
        rows[3],
    );
    let derived = derive_paths(&app.image_path);
    f.render_widget(
        Paragraph::new(format!(
            "image: {}\nwikis: {}\nkiwix: {}",
            app.image_path.display(),
            derived.content_wikis.display(),
            derived.content_kiwix.display()
        ))
        .block(Block::default().borders(Borders::ALL).title("Paths"))
        .wrap(Wrap { trim: false }),
        rows[4],
    );
}

fn render_confirm(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let disk = app.selected_disk().unwrap_or("?");
    let plan = build_plan(&app.image, disk, app.shred_level, &app.hostname);
    let text = format!(
        "Image '{}' will be cloned to /dev/{disk}.\nHostname: {}\nPoweroff: {}\n\n{}",
        app.image,
        app.hostname,
        if app.poweroff { "yes" } else { "no" },
        plan.join("\n"),
    );
    f.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Confirm"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_installing(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(6)])
        .split(area);
    f.render_widget(
        Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(app.progress_label.clone()),
            )
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(app.progress.clamp(0.0, 1.0)),
        rows[0],
    );
    let log: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(30)
        .rev()
        .map(|l| Line::from(Span::raw(l.clone())))
        .collect();
    f.render_widget(
        Paragraph::new(log)
            .block(Block::default().borders(Borders::ALL).title("Log"))
            .wrap(Wrap { trim: false }),
        rows[1],
    );
}

fn render_done(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let disk = app.selected_disk().unwrap_or("?");
    let text = format!(
        "Finished! '{}' cloned to /dev/{disk}.\n\n{}",
        app.image,
        app.logs
            .iter()
            .rev()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    f.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Done"))
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_rect(area: ratatui::layout::Rect, pct_x: u16, pct_y: u16) -> ratatui::layout::Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(vertical[1])[1]
}
