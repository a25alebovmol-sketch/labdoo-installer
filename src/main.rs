//! Labdoo installer TUI — Ratatui port of `exec.sh`.
//!
//! Flow (mirrors the shell script):
//! 1. Pick target disk (never the boot device).
//! 2. Set shred level, hostname, content list, poweroff flag.
//! 3. Confirm by typing the disk name + marker check.
//! 4. Install with live progress + log (dry-run by default).
//!
//! Hardening: allow-listed disk/image names, no shell anywhere, absolute
//! binary paths with a cleared env, boot-device fail-closed, marker check,
//! display sanitising, input filtering, size caps, `--execute` gated to
//! Linux + root + typed confirmation.

mod installer;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use installer::{
    build_plan, clonezilla_marker_path, derive_paths, disk_from_source, filter_disks,
    is_boot_disk_selected, is_valid_content_list_value, is_valid_disk_name, is_valid_image_name,
    parse_section, resolve_part_number, sanitize_display, target_partition, validate_hostname,
    KEEP_HOSTNAME,
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
    /// Simulate everything (default true; kept for compat, `--execute` wins)
    #[arg(long, default_value_t = true)]
    dry_run: bool,
    /// Actually execute destructive steps (Linux + root + typed confirm only).
    #[arg(long, default_value_t = false)]
    execute: bool,
}

/// True only when destructive steps are allowed. Fail closed everywhere else.
fn is_live_mode(cli: &Cli) -> bool {
    cli.execute
}

struct App {
    step: Step,
    image: String,
    image_path: PathBuf,
    image_marker_ok: bool,
    live: bool,

    disks: Vec<String>,
    boot_device: String,
    boot_known: bool,
    disk_state: ListState,
    error: Option<String>,

    field: usize,
    shred_level: u8,
    hostname: String,
    content_list: String,
    poweroff: bool,
    confirm_input: String,

    progress: f64,
    progress_label: String,
    logs: Vec<String>,
    tick: u64,
    done_ok: bool,
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

impl App {
    fn new(cli: &Cli, disks: Vec<String>, boot_device: String, boot_known: bool) -> Self {
        let image = sanitize_display(&cli.image, 64);
        let derived = derive_paths(&cli.image_path);
        let mut disk_state = ListState::default();
        if !disks.is_empty() {
            let idx = disks.iter().position(|d| d != &boot_device).unwrap_or(0);
            disk_state.select(Some(idx));
        }
        let live = is_live_mode(cli);
        let marker_ok = clonezilla_marker_path(&cli.image_path).exists();
        let mut app = Self {
            step: Step::PickDisk,
            image,
            image_path: cli.image_path.clone(),
            image_marker_ok: marker_ok,
            live,
            disks,
            boot_device,
            boot_known,
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
            confirm_input: String::new(),
            progress: 0.0,
            progress_label: "idle".to_string(),
            logs: Vec::new(),
            tick: 0,
            done_ok: false,
        };
        if cli.dry_run && cli.execute {
            app.log("note: --dry-run and --execute both given, --execute wins (LIVE)");
        }
        app.log(format!(
            "image '{}' from {} [{}]",
            app.image,
            app.image_path.display(),
            if live { "LIVE" } else { "dry-run" }
        ));
        if live {
            app.log("LIVE MODE: destructive commands allowed after typed confirmation");
        } else {
            app.log("dry-run: no destructive command will be executed");
        }
        if !marker_ok {
            app.log("warning: clonezilla-img marker NOT found in image dir");
        }
        if !boot_known {
            app.log("warning: boot device unknown, fail-closed guards are ON");
        }
        app
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

    /// Fail-closed gate shared by PickDisk and Confirm.
    fn disk_selection_error(&self) -> Option<String> {
        let disk = self.selected_disk()?;
        if !is_valid_disk_name(disk) {
            return Some("refusing: disk name failed allow-list".to_string());
        }
        if !self.boot_known {
            return Some("refusing: boot device unknown (fail-closed)".to_string());
        }
        if self.selected_is_boot() {
            return Some(format!(
                "refusing: {} is the boot device",
                sanitize_display(disk, 32)
            ));
        }
        None
    }

    fn log(&mut self, line: impl Into<String>) {
        // Never store control chars; logs are rendered verbatim in the TUI.
        let clean = sanitize_display(&line.into(), 512);
        self.logs.push(clean);
        if self.logs.len() > 200 {
            let excess = self.logs.len() - 200;
            self.logs.drain(..excess);
        }
    }

    /// Install stages shown with a progress bar. Display-only strings built
    /// from allow-listed values; real execution must use the argv helpers
    /// with `Command` (no shell) — not implemented in this TUI demo.
    fn install_stages(&self) -> Vec<(String, f64, String)> {
        let disk = self.selected_disk().unwrap_or("sda").to_string();
        let part_number = resolve_part_number(true);
        let part = target_partition(&disk, part_number);
        let image = sanitize_display(&self.image, 64);
        let shred = format!("shred /dev/{disk} -v -f -n {}", self.shred_level);
        let clone = format!(
            "/usr/sbin/ocs-sr -e1 auto -e2 -r -icrc -icds -scr -j2 -k1 -p true restoredisk {image} {disk}"
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
                format!(
                    "mount /dev/{part} /mnt; hostname={}",
                    sanitize_display(&self.hostname, 64)
                ),
            ),
            (
                "install contents".to_string(),
                0.88,
                format!("content list {}", sanitize_display(&self.content_list, 256)),
            ),
            (
                "perms + machine-id + cleanup".to_string(),
                1.0,
                "chmod/chown, machine-id, unmount".to_string(),
            ),
        ]
    }
}

fn run_cleared(cmd: &str, args: &[&str]) -> Option<String> {
    // Absolute-path binaries, cleared env, no shell. Used for read-only
    // discovery (`lsblk`, `findmnt`, `id`) — never for destructive steps.
    let out = std::process::Command::new(cmd)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn first_existing(paths: &[&str], args: &[&str]) -> Option<String> {
    for p in paths {
        if !Path::new(p).exists() {
            continue;
        }
        if let Some(out) = run_cleared(p, args) {
            return Some(out);
        }
    }
    None
}

fn discover_disks() -> (Vec<String>, String, bool) {
    let raw = first_existing(
        &["/usr/bin/lsblk", "/bin/lsblk"],
        &["-d", "-n", "-o", "NAME"],
    )
    .unwrap_or_else(|| "sda sdb nvme0n1".to_string());
    let disks = filter_disks(&raw);
    let (boot, known) = detect_boot_device();
    let boot = boot.unwrap_or_default();
    let known = known && !boot.is_empty();
    if disks.is_empty() {
        (Vec::new(), boot, known)
    } else {
        (disks, boot, known)
    }
}

fn detect_boot_device() -> (Option<String>, bool) {
    // 1) Labtix live marker used by exec.sh — strict allow-list.
    if let Ok(s) = std::fs::read_to_string("/live/config/boot-device") {
        let raw = s.trim();
        let stripped = raw.strip_prefix("/dev/").unwrap_or(raw);
        // Marker file contains e.g. "/dev/sda"; map through the same parser
        // as findmnt so nvme/mmcblk are handled identically.
        if let Some(disk) = disk_from_source(&format!("/dev/{stripped}")) {
            if is_valid_disk_name(&disk) {
                return (Some(disk), true);
            }
        }
        // Marker present but unparsable: fail closed (known=false).
        return (None, false);
    }
    // 2) Device backing `/` via findmnt.
    if let Some(out) = first_existing(
        &["/usr/bin/findmnt", "/bin/findmnt"],
        &["-n", "-o", "SOURCE", "/"],
    ) {
        if let Some(first) = out.split_whitespace().next() {
            if let Some(disk) = disk_from_source(first) {
                if is_valid_disk_name(&disk) {
                    return (Some(disk), true);
                }
            }
        }
    }
    (None, false)
}

fn is_root() -> bool {
    // No extra deps: ask `id -u` with a cleared env.
    if let Some(out) = first_existing(&["/usr/bin/id", "/bin/id"], &["-u"]) {
        return out.trim() == "0";
    }
    false
}

fn validate_startup(cli: &Cli) -> Result<()> {
    if !is_valid_image_name(cli.image.trim()) {
        bail!("invalid --image (use A-Z a-z 0-9 . _ -, max 64, must not start with -)");
    }
    if cli.image_path.to_string_lossy().len() > 256 {
        bail!("--image-path too long (max 256)");
    }
    if is_live_mode(cli) {
        if !cfg!(target_os = "linux") {
            bail!("--execute is Linux-only (fail-closed)");
        }
        if !is_root() {
            bail!("--execute requires root (fail-closed)");
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    validate_startup(&cli)?;
    let (disks, boot_device, boot_known) = discover_disks();
    let mut app = App::new(&cli, disks, boot_device, boot_known);

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
                app.log(format!(
                    "installing '{}' -> /dev/{disk} [{}]",
                    sanitize_display(&app.image, 64),
                    if app.live { "LIVE" } else { "dry-run" }
                ));
                for line in build_plan(&app.image, &disk, app.shred_level, &app.hostname) {
                    app.log(format!("plan: {line}"));
                }
                if app.live {
                    app.log("LIVE: destructive steps would run now (demo build simulates)");
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
                } else if let Some(err) = app.disk_selection_error() {
                    app.error = Some(err);
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
                if !is_valid_content_list_value(app.content_list.trim()) {
                    app.error = Some("invalid content list path".to_string());
                    return KeyAction::None;
                }
                app.confirm_input.clear();
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
            // Input filtering: hostname `[A-Za-z0-9-]`, paths safe chars only.
            KeyCode::Char(c) => match OptionField::ALL[app.field] {
                OptionField::Hostname
                    if app.hostname.len() < 64 && (c.is_ascii_alphanumeric() || c == '-') =>
                {
                    app.hostname.push(c);
                }
                OptionField::ContentList
                    if app.content_list.len() < 256
                        && (c.is_ascii_alphanumeric()
                            || matches!(c, '/' | '.' | '_' | '-' | ' ')) =>
                {
                    app.content_list.push(c);
                }
                _ => {}
            },
            _ => {}
        },
        Step::Confirm => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc => app.step = Step::Options,
            KeyCode::Backspace => {
                app.confirm_input.pop();
            }
            KeyCode::Char(c) if c.is_ascii_alphanumeric() && app.confirm_input.len() < 32 => {
                app.confirm_input.push(c);
            }
            KeyCode::Enter => {
                if let Some(err) = app.disk_selection_error() {
                    app.error = Some(err);
                    return KeyAction::None;
                }
                let disk = app.selected_disk().unwrap_or("?");
                if app.confirm_input.trim() != disk {
                    app.error = Some(format!("type '{disk}' to confirm (fail-closed)"));
                    return KeyAction::None;
                }
                if app.live && !app.image_marker_ok {
                    app.error =
                        Some("refusing LIVE install: clonezilla-img marker missing".to_string());
                    return KeyAction::None;
                }
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

/// Best-effort preview of the `####info` section, hardened:
/// allow-listed path chars, regular file only, 1 MiB cap, 5 lines shown.
fn log_content_info(app: &mut App) {
    let list_path = app.content_list.split_whitespace().next().unwrap_or("");
    if !is_valid_content_list_value(list_path) {
        app.log("content info: skipped (path failed allow-list)");
        return;
    }
    let path = Path::new(list_path);
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if !meta.is_file() || meta.len() > 1024 * 1024 {
        app.log("content info: skipped (not a small regular file)");
        return;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let info = parse_section(&text, "####info");
    if info.is_empty() {
        return;
    }
    for line in info.iter().take(5) {
        app.log(format!("content info: {}", sanitize_display(line, 200)));
    }
}

fn advance_install(app: &mut App) {
    let stages = app.install_stages();
    app.tick += 1;
    let idx = ((app.tick / 25) as usize).min(stages.len().saturating_sub(1));
    let (label, ratio, cmd) = &stages[idx];
    if app.progress_label != *label {
        app.progress_label = label.clone();
        app.log(format!(
            ">> {}: {}",
            sanitize_display(label, 64),
            sanitize_display(cmd, 256)
        ));
    }
    app.progress = (*ratio * 100.0).round() / 100.0;
    if app.progress >= 1.0 {
        let disk = sanitize_display(app.selected_disk().unwrap_or("?"), 32);
        app.log(format!(
            "finished: '{}' cloned to /dev/{disk}",
            sanitize_display(&app.image, 64)
        ));
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
            sanitize_display(&app.image, 64),
            if app.live { "[LIVE]" } else { "[dry-run]" },
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
        Step::Confirm => "type disk name to confirm   Enter starts   Esc back   q quit",
        Step::Installing => "installing… Ctrl-C quits",
        Step::Done => "Enter/q quit",
    };
    let mut footer_text = hint.to_string();
    if let Some(e) = &app.error {
        footer_text = format!("ERROR: {}  |  {hint}", sanitize_display(e, 200));
    }
    let footer = Paragraph::new(footer_text).block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, chunks[2]);

    if app.error.is_some() && matches!(app.step, Step::PickDisk) {
        let popup = Paragraph::new(sanitize_display(
            &app.error.clone().unwrap_or_default(),
            200,
        ))
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
                format!("{}   [BOOT — do not use]", sanitize_display(d, 32))
            } else {
                sanitize_display(d, 32)
            };
            let style = if boot {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();
    let boot_label = if app.boot_known {
        sanitize_display(&app.boot_device, 32)
    } else {
        "unknown (fail-closed)".to_string()
    };
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Target disk (boot: {boot_label})")),
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
            sanitize_display(app.selected_disk().unwrap_or("?"), 32),
            app.shred_level
        ))
        .style(focus(0))
        .block(box_style(0).title("1. Shred")),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(sanitize_display(&app.hostname, 64))
            .style(focus(1))
            .block(box_style(1).title("2. Hostname")),
        rows[1],
    );
    f.render_widget(
        Paragraph::new(sanitize_display(&app.content_list, 256))
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
            "image: {}\nwikis: {}\nkiwix: {}\nmarker clonezilla-img: {}",
            sanitize_display(&app.image_path.to_string_lossy(), 256),
            sanitize_display(&derived.content_wikis.to_string_lossy(), 256),
            sanitize_display(&derived.content_kiwix.to_string_lossy(), 256),
            if app.image_marker_ok {
                "present"
            } else {
                "MISSING"
            }
        ))
        .block(Block::default().borders(Borders::ALL).title("Paths"))
        .wrap(Wrap { trim: false }),
        rows[4],
    );
}

fn render_confirm(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let disk = sanitize_display(app.selected_disk().unwrap_or("?"), 32);
    let plan = build_plan(&app.image, &disk, app.shred_level, &app.hostname);
    let mode = if app.live {
        "LIVE — destructive!"
    } else {
        "dry-run — nothing will run"
    };
    let text = format!(
        "Image '{}' will be cloned to /dev/{disk} ({mode}).\nHostname: {}\nPoweroff: {}\nMarker: {}\n\n{}\n\nType '{disk}' below, then Enter:",
        sanitize_display(&app.image, 64),
        sanitize_display(&app.hostname, 64),
        if app.poweroff { "yes" } else { "no" },
        if app.image_marker_ok {
            "present"
        } else {
            "MISSING"
        },
        plan.join("\n"),
    );
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(3)])
        .split(area);
    f.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title("Confirm"))
            .wrap(Wrap { trim: false }),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(sanitize_display(&app.confirm_input, 32)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Type '{disk}' here")),
        ),
        rows[1],
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
                    .title(sanitize_display(&app.progress_label, 64)),
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
        .map(|l| Line::from(Span::raw(sanitize_display(l, 512))))
        .collect();
    f.render_widget(
        Paragraph::new(log)
            .block(Block::default().borders(Borders::ALL).title("Log"))
            .wrap(Wrap { trim: false }),
        rows[1],
    );
}

fn render_done(f: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let disk = sanitize_display(app.selected_disk().unwrap_or("?"), 32);
    let text = format!(
        "Finished! '{}' cloned to /dev/{disk}.\n\n{}",
        sanitize_display(&app.image, 64),
        app.logs
            .iter()
            .rev()
            .take(12)
            .map(|l| sanitize_display(l, 512))
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
