//! Labdoo Installer: a safe preflight TUI for Labtix Clonezilla images.
//!
//! This release deliberately does not write disks. The old interface showed a
//! simulated progress bar even with --execute and then reported success. A
//! destructive installer must never claim that an install happened unless an
//! audited executor has actually completed every stage.

mod installer;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use installer::{
    boot_device_from_source, build_plan, clonezilla_marker_path, derive_paths,
    is_valid_content_list_value, is_valid_image_name, parse_disk_inventory, parse_section,
    sanitize_display, target_disk_error, validate_hostname, BootDevice, DiskInfo, KEEP_HOSTNAME,
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

const MAX_LOG_LINES: usize = 160;
const MAX_CONTENT_LIST_BYTES: u64 = 1024 * 1024;

#[derive(Parser, Debug)]
#[command(
    name = "labdoo-installer",
    version,
    about = "Safe Labdoo/Labtix Clonezilla installer preflight"
)]
struct Cli {
    /// Clonezilla image name, for example labdoo-24.04.
    #[arg(long, default_value = "labdoo-image")]
    image: String,

    /// Absolute path to the selected Clonezilla image directory.
    #[arg(long, default_value = "/home/partimag/labdoo-image")]
    image_path: PathBuf,

    /// Kept for command-line compatibility. Preview mode is already the default.
    #[arg(long)]
    dry_run: bool,

    /// Rejected intentionally: this build only creates a verified plan.
    #[arg(long)]
    execute: bool,
}

#[derive(Debug, Clone)]
struct Discovery {
    disks: Vec<DiskInfo>,
    boot_device: Option<BootDevice>,
    boot_source: Option<String>,
    notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    PickDisk,
    Options,
    Confirm,
    Plan,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    None,
    Quit,
    Refresh,
}

struct App {
    step: Step,
    image: String,
    image_path: PathBuf,
    image_marker_ok: bool,

    disks: Vec<DiskInfo>,
    boot_device: Option<BootDevice>,
    boot_source: Option<String>,
    discovery_notes: Vec<String>,
    disk_state: ListState,

    field: usize,
    shred_level: u8,
    hostname: String,
    content_list: String,
    poweroff: bool,
    confirm_input: String,
    error: Option<String>,
    show_help: bool,
    logs: Vec<String>,
}

impl App {
    fn new(cli: &Cli, discovery: Discovery) -> Self {
        let image = sanitize_display(&cli.image, 64);
        let image_path = cli.image_path.clone();
        let content_list = derive_paths(&image_path)
            .content_lists
            .join("none")
            .to_string_lossy()
            .into_owned();
        let mut app = Self {
            step: Step::PickDisk,
            image,
            image_marker_ok: clonezilla_marker_path(&image_path).is_file(),
            image_path,
            disks: discovery.disks,
            boot_device: discovery.boot_device,
            boot_source: discovery.boot_source,
            discovery_notes: discovery.notes,
            disk_state: ListState::default(),
            field: 0,
            shred_level: 1,
            hostname: KEEP_HOSTNAME.to_string(),
            content_list,
            poweroff: false,
            confirm_input: String::new(),
            error: None,
            show_help: false,
            logs: Vec::new(),
        };
        app.select_preferred_disk(None);
        app.log("SAFE PREVIEW: this build will not change any disk");
        app.log(format!("image: {}", app.image_path.display()));
        match &app.boot_device {
            Some(device) => app.log(format!("boot media: {}", device.description())),
            None => {
                app.log("warning: boot medium could not be identified; target selection is blocked")
            }
        }
        if !app.image_marker_ok {
            app.log("warning: clonezilla-img marker is missing from the image directory");
        }
        for note in app.discovery_notes.clone() {
            app.log(format!("diagnostic: {note}"));
        }
        app
    }

    fn selected_disk(&self) -> Option<&DiskInfo> {
        self.disk_state
            .selected()
            .and_then(|index| self.disks.get(index))
    }

    fn disk_selection_error(&self) -> Option<String> {
        target_disk_error(self.boot_device.as_ref(), self.selected_disk())
    }

    fn select_preferred_disk(&mut self, preferred_name: Option<&str>) {
        if self.disks.is_empty() {
            self.disk_state.select(None);
            return;
        }
        let selected = preferred_name
            .and_then(|name| self.disks.iter().position(|disk| disk.name == name))
            .or_else(|| {
                self.disks.iter().position(|disk| {
                    !disk.read_only
                        && self
                            .boot_device
                            .as_ref()
                            .map_or(true, |boot| !boot.blocks_target(&disk.name))
                })
            })
            .unwrap_or(0);
        self.disk_state.select(Some(selected));
    }

    fn refresh(&mut self) {
        let previous = self.selected_disk().map(|disk| disk.name.clone());
        let discovery = discover_system();
        self.disks = discovery.disks;
        self.boot_device = discovery.boot_device;
        self.boot_source = discovery.boot_source;
        self.discovery_notes = discovery.notes;
        self.image_marker_ok = clonezilla_marker_path(&self.image_path).is_file();
        self.select_preferred_disk(previous.as_deref());
        self.log("hardware inventory refreshed");
        match &self.boot_device {
            Some(device) => self.log(format!("boot media: {}", device.description())),
            None => self.log("warning: boot medium remains unknown"),
        }
    }

    fn log(&mut self, message: impl Into<String>) {
        self.logs.push(sanitize_display(&message.into(), 512));
        if self.logs.len() > MAX_LOG_LINES {
            let excess = self.logs.len() - MAX_LOG_LINES;
            self.logs.drain(..excess);
        }
    }

    fn load_content_info(&mut self) {
        let path_text = self.content_list.trim();
        if !is_valid_content_list_value(path_text) {
            self.log("content info: skipped because the path is invalid");
            return;
        }
        let path = Path::new(path_text);
        let Ok(metadata) = std::fs::metadata(path) else {
            self.log("content info: list file is not currently available");
            return;
        };
        if !metadata.is_file() || metadata.len() > MAX_CONTENT_LIST_BYTES {
            self.log("content info: skipped because it is not a small regular file");
            return;
        }
        let Ok(contents) = std::fs::read_to_string(path) else {
            self.log("content info: list file is not valid UTF-8");
            return;
        };
        let info = parse_section(&contents, "####info");
        if info.is_empty() {
            self.log("content info: no ####info section");
        } else {
            for line in info.iter().take(8) {
                self.log(format!("content: {line}"));
            }
        }
    }

    fn plan(&mut self) {
        let Some(disk) = self.selected_disk().map(|disk| disk.name.clone()) else {
            return;
        };
        self.log(format!("verified target: /dev/{disk}"));
        for line in build_plan(&self.image, &disk, self.shred_level, &self.hostname) {
            self.log(format!("plan: {line}"));
        }
        self.load_content_info();
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    validate_startup(&cli)?;
    let discovery = discover_system();
    let mut app = App::new(&cli, discovery);

    enable_raw_mode().context("enable raw terminal mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("open terminal interface")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;
    terminal.clear().ok();

    let result = run(&mut terminal, &mut app);

    disable_raw_mode().ok();
    execute!(io::stdout(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

fn validate_startup(cli: &Cli) -> Result<()> {
    if cli.execute {
        bail!(
            "--execute is disabled in this safety-preview build. It will never pretend to have installed an image; use this release to validate the image, boot medium, target disk and command plan."
        );
    }
    if !is_valid_image_name(cli.image.trim()) {
        bail!("invalid --image; use letters, numbers, dots, underscores and hyphens only");
    }
    if !is_absolute_image_path(&cli.image_path) {
        bail!("--image-path must be absolute");
    }
    if cli.image_path.to_string_lossy().len() > 256 {
        bail!("--image-path is too long");
    }
    Ok(())
}

/// Accept POSIX absolute paths while developing on Windows too. The deployed
/// Labtix program runs on Linux, where Path::is_absolute already covers them.
fn is_absolute_image_path(path: &Path) -> bool {
    path.is_absolute() || path.to_string_lossy().starts_with('/')
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal
            .draw(|frame| ui(frame, app))
            .context("draw interface")?;
        let event = event::read().context("read keyboard input")?;
        if is_force_quit(&event) {
            return Ok(());
        }
        match handle_key(app, &event) {
            KeyAction::None => {}
            KeyAction::Quit => return Ok(()),
            KeyAction::Refresh => app.refresh(),
        }
    }
}

fn is_force_quit(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(key)
            if key.kind == KeyEventKind::Press
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
    )
}

fn handle_key(app: &mut App, event: &Event) -> KeyAction {
    let Event::Key(key) = event else {
        return KeyAction::None;
    };
    if key.kind != KeyEventKind::Press {
        return KeyAction::None;
    }

    if app.show_help {
        match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('h') => app.show_help = false,
            _ => app.show_help = false,
        }
        return KeyAction::None;
    }
    if matches!(key.code, KeyCode::Char('?') | KeyCode::Char('h')) {
        app.show_help = true;
        return KeyAction::None;
    }
    app.error = None;

    match app.step {
        Step::PickDisk => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Char('r') => return KeyAction::Refresh,
            KeyCode::Up | KeyCode::Char('k') => move_disk_selection(app, false),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => move_disk_selection(app, true),
            KeyCode::Enter | KeyCode::Char('l') => {
                if let Some(error) = app.disk_selection_error() {
                    app.error = Some(error);
                } else {
                    app.step = Step::Options;
                }
            }
            _ => {}
        },
        Step::Options => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc => app.step = Step::PickDisk,
            KeyCode::Tab | KeyCode::Down => app.field = (app.field + 1) % OptionField::ALL.len(),
            KeyCode::Up => {
                app.field = (app.field + OptionField::ALL.len() - 1) % OptionField::ALL.len()
            }
            KeyCode::Left | KeyCode::Right if OptionField::ALL[app.field] == OptionField::Shred => {
                app.shred_level = if key.code == KeyCode::Left {
                    app.shred_level.saturating_sub(1)
                } else {
                    (app.shred_level + 1).min(3)
                };
            }
            KeyCode::Char(' ') if OptionField::ALL[app.field] == OptionField::Poweroff => {
                app.poweroff = !app.poweroff;
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
            KeyCode::Char(character) => match OptionField::ALL[app.field] {
                OptionField::Hostname
                    if app.hostname.len() < 63
                        && (character.is_ascii_alphanumeric() || character == '-') =>
                {
                    app.hostname.push(character);
                }
                OptionField::ContentList
                    if app.content_list.len() < 256
                        && (character.is_ascii_alphanumeric()
                            || matches!(character, '/' | '.' | '_' | '-' | ' ')) =>
                {
                    app.content_list.push(character);
                }
                _ => {}
            },
            KeyCode::Enter => {
                if !validate_hostname(&app.hostname) {
                    app.error = Some(
                        "invalid hostname: use A-Z, 0-9 and hyphens; no leading/trailing hyphen"
                            .to_string(),
                    );
                } else if !is_valid_content_list_value(app.content_list.trim()) {
                    app.error = Some("content list must be an absolute safe path".to_string());
                } else {
                    app.confirm_input.clear();
                    app.step = Step::Confirm;
                }
            }
            _ => {}
        },
        Step::Confirm => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc => app.step = Step::Options,
            KeyCode::Backspace => {
                app.confirm_input.pop();
            }
            KeyCode::Char(character)
                if (character.is_ascii_lowercase() || character.is_ascii_digit())
                    && app.confirm_input.len() < 32 =>
            {
                app.confirm_input.push(character);
            }
            KeyCode::Enter => {
                if let Some(error) = app.disk_selection_error() {
                    app.error = Some(error);
                } else if !app.image_marker_ok {
                    app.error = Some(
                        "clonezilla-img marker is missing from the selected image directory"
                            .to_string(),
                    );
                } else if app.confirm_input
                    != app
                        .selected_disk()
                        .map(|disk| disk.name.as_str())
                        .unwrap_or_default()
                {
                    let target = app
                        .selected_disk()
                        .map(|disk| disk.name.as_str())
                        .unwrap_or("?");
                    app.error = Some(format!("type '{target}' exactly to confirm"));
                } else {
                    app.plan();
                    app.step = Step::Plan;
                }
            }
            _ => {}
        },
        Step::Plan => match key.code {
            KeyCode::Char('q') => return KeyAction::Quit,
            KeyCode::Esc => app.step = Step::Confirm,
            KeyCode::Char('r') => return KeyAction::Refresh,
            _ => {}
        },
    }
    KeyAction::None
}

fn move_disk_selection(app: &mut App, forward: bool) {
    let length = app.disks.len();
    if length == 0 {
        return;
    }
    let current = app.disk_state.selected().unwrap_or(0);
    let next = if forward {
        (current + 1) % length
    } else {
        (current + length - 1) % length
    };
    app.disk_state.select(Some(next));
}

fn run_cleared(command: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(command)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn first_existing(paths: &[&str], args: &[&str]) -> Option<String> {
    paths
        .iter()
        .find(|path| Path::new(**path).is_file())
        .and_then(|path| run_cleared(path, args))
}

fn source_for_mountpoint(mountpoint: &str) -> Option<String> {
    let binaries = ["/usr/bin/findmnt", "/bin/findmnt"];
    first_existing(&binaries, &["-n", "-o", "SOURCE", "--target", mountpoint])
        .or_else(|| first_existing(&binaries, &["-n", "-o", "SOURCE", mountpoint]))
        .and_then(|output| output.split_whitespace().next().map(str::to_string))
}

fn normalized_source(source: &str) -> String {
    if !source.starts_with("/dev/") {
        return source.to_string();
    }
    std::fs::canonicalize(source)
        .ok()
        .and_then(|path| path.into_os_string().into_string().ok())
        .unwrap_or_else(|| source.to_string())
}

fn discover_boot_device(notes: &mut Vec<String>) -> (Option<BootDevice>, Option<String>) {
    let mut candidates = Vec::new();
    match std::fs::read_to_string("/live/config/boot-device") {
        Ok(value) if !value.trim().is_empty() => {
            candidates.push(("Labtix boot marker", value.trim().to_string()));
        }
        Ok(_) => notes.push("Labtix boot marker is empty".to_string()),
        Err(_) => {}
    }
    for mountpoint in [
        "/live/boot-dev",
        "/run/live/boot-dev",
        "/run/live/medium",
        "/cdrom",
        "/",
    ] {
        if let Some(source) = source_for_mountpoint(mountpoint) {
            candidates.push((mountpoint, source));
        }
    }

    for (origin, source) in candidates {
        let normalized = normalized_source(&source);
        if let Some(device) = boot_device_from_source(&normalized) {
            notes.push(format!(
                "boot source found via {origin}: {}",
                sanitize_display(&normalized, 128)
            ));
            return (Some(device), Some(normalized));
        }
    }
    notes.push("could not map a live mount to a safe block device".to_string());
    (None, None)
}

fn discover_system() -> Discovery {
    let mut notes = Vec::new();
    let raw_disks = first_existing(
        &["/usr/bin/lsblk", "/bin/lsblk"],
        &["-d", "-n", "-P", "-o", "NAME,SIZE,MODEL,TRAN,RM,RO,TYPE"],
    );
    let disks = match raw_disks {
        Some(raw) => {
            let disks = parse_disk_inventory(&raw);
            if disks.is_empty() {
                notes.push("lsblk found no target-capable disks".to_string());
            }
            disks
        }
        None => {
            notes.push("could not run lsblk; no disks have been invented".to_string());
            Vec::new()
        }
    };
    let (boot_device, boot_source) = discover_boot_device(&mut notes);
    Discovery {
        disks,
        boot_device,
        boot_source,
        notes,
    }
}

fn ui(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(3),
        ])
        .split(area);

    render_title(frame, app, layout[0]);
    match app.step {
        Step::PickDisk => render_pick_disk(frame, app, layout[1]),
        Step::Options => render_options(frame, app, layout[1]),
        Step::Confirm => render_confirm(frame, app, layout[1]),
        Step::Plan => render_plan(frame, app, layout[1]),
    }
    render_footer(frame, app, layout[2]);

    if let Some(error) = &app.error {
        render_popup(frame, "Safety check", error, Color::Red);
    } else if app.show_help {
        render_popup(frame, "Help", &help_text(app.step), Color::Cyan);
    }
}

fn render_title(frame: &mut Frame, app: &App, area: Rect) {
    let step = match app.step {
        Step::PickDisk => "1/4 target",
        Step::Options => "2/4 options",
        Step::Confirm => "3/4 confirm",
        Step::Plan => "4/4 verified plan",
    };
    let title = Paragraph::new(Line::from(vec![
        Span::styled(
            " Labdoo Installer ",
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "SAFE PREVIEW",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("  '{}'  {step}", sanitize_display(&app.image, 64))),
    ]))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, area);
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect) {
    let hint = match app.step {
        Step::PickDisk => "↑/↓ select  Enter next  r refresh  ? help  q quit",
        Step::Options => "Tab move  ←/→ shred  Space toggle  Enter next  Esc back  ? help",
        Step::Confirm => "type the disk name  Enter review plan  Esc back  ? help",
        Step::Plan => "Esc back  r refresh  q quit  ? help",
    };
    frame.render_widget(
        Paragraph::new(hint).block(Block::default().borders(Borders::ALL)),
        area,
    );
}

fn render_pick_disk(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(4),
            Constraint::Length(5),
        ])
        .split(area);
    let boot = app
        .boot_device
        .as_ref()
        .map(BootDevice::description)
        .unwrap_or_else(|| "UNKNOWN — selection is blocked".to_string());
    let boot_detail = app
        .boot_source
        .as_deref()
        .map(|source| format!("\nsource: {}", sanitize_display(source, 128)))
        .unwrap_or_default();
    frame.render_widget(
        Paragraph::new(format!(
            "Boot medium: {boot}{boot_detail}\nSelect the disk that will be erased. Read-only disks are refused."
        ))
        .block(Block::default().borders(Borders::ALL).title("Safety status"))
        .wrap(Wrap { trim: false }),
        rows[0],
    );

    let items: Vec<ListItem> = app
        .disks
        .iter()
        .map(|disk| {
            let is_boot = app
                .boot_device
                .as_ref()
                .is_some_and(|boot| boot.blocks_target(&disk.name));
            let mut label = disk.label();
            if is_boot {
                label.push_str("  [BOOT — REFUSED]");
            }
            let style = if is_boot || disk.read_only {
                Style::default().fg(Color::Red)
            } else if disk.removable {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(Span::styled(label, style)))
        })
        .collect();
    let mut state = app.disk_state.clone();
    frame.render_stateful_widget(
        List::new(items)
            .block(Block::default().borders(Borders::ALL).title("Target disks"))
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> "),
        rows[1],
        &mut state,
    );

    let notes = if app.discovery_notes.is_empty() {
        "No diagnostics.".to_string()
    } else {
        app.discovery_notes
            .iter()
            .take(3)
            .map(|note| format!("• {}", sanitize_display(note, 180)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    frame.render_widget(
        Paragraph::new(notes)
            .block(Block::default().borders(Borders::ALL).title("Discovery"))
            .wrap(Wrap { trim: false }),
        rows[2],
    );
}

fn render_options(frame: &mut Frame, app: &App, area: Rect) {
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
    let focus = |index: usize| {
        if app.field == index {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    let block = |index: usize, title: &str| {
        let mut block = Block::default()
            .borders(Borders::ALL)
            .title(title.to_string());
        if app.field == index {
            block = block.border_style(Style::default().fg(Color::Yellow));
        }
        block
    };
    let target = app
        .selected_disk()
        .map(|disk| disk.name.as_str())
        .unwrap_or("?");
    frame.render_widget(
        Paragraph::new(format!(
            "Passes for /dev/{target}: {}  (0 = no overwrite, 3 = strongest legacy setting)",
            app.shred_level
        ))
        .style(focus(0))
        .block(block(0, "1. Shred")),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(sanitize_display(&app.hostname, 63))
            .style(focus(1))
            .block(block(1, "2. Hostname")),
        rows[1],
    );
    frame.render_widget(
        Paragraph::new(sanitize_display(&app.content_list, 256))
            .style(focus(2))
            .block(block(2, "3. Content-list file")),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(if app.poweroff {
            "[X] power off after a future installation  [Space]"
        } else {
            "[ ] power off after a future installation  [Space]"
        })
        .style(focus(3))
        .block(block(3, "4. After install")),
        rows[3],
    );

    let paths = derive_paths(&app.image_path);
    let marker = if app.image_marker_ok {
        "present"
    } else {
        "MISSING"
    };
    frame.render_widget(
        Paragraph::new(format!(
            "Image directory: {}\nMarker clonezilla-img: {marker}\nWiki archive: {}\nKiwix archive: {}",
            sanitize_display(&app.image_path.to_string_lossy(), 180),
            sanitize_display(&paths.content_wikis.to_string_lossy(), 180),
            sanitize_display(&paths.content_kiwix.to_string_lossy(), 180),
        ))
        .block(Block::default().borders(Borders::ALL).title("Preflight paths"))
        .wrap(Wrap { trim: false }),
        rows[4],
    );
}

fn render_confirm(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(3)])
        .split(area);
    let target = app.selected_disk();
    let target_name = target.map(|disk| disk.name.as_str()).unwrap_or("?");
    let target_label = target
        .map(DiskInfo::label)
        .unwrap_or_else(|| "?".to_string());
    let removable_note = target
        .filter(|disk| disk.removable)
        .map(|_| "\nWARNING: this is removable media. Verify it is intended.")
        .unwrap_or_default();
    let plan = build_plan(&app.image, target_name, app.shred_level, &app.hostname).join("\n");
    frame.render_widget(
        Paragraph::new(format!(
            "This is a non-destructive verified preview.\n\nTarget: {target_label}\nImage marker: {}\nHostname: {}\nPoweroff requested: {}{removable_note}\n\nPlanned operations:\n{plan}\n\nType '{target_name}' exactly below to review the plan.",
            if app.image_marker_ok { "present" } else { "MISSING" },
            sanitize_display(&app.hostname, 63),
            if app.poweroff { "yes" } else { "no" },
        ))
        .block(Block::default().borders(Borders::ALL).title("Confirm target"))
        .wrap(Wrap { trim: false }),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(sanitize_display(&app.confirm_input, 32)).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Type '{target_name}'")),
        ),
        rows[1],
    );
}

fn render_plan(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(5)])
        .split(area);
    let target = app
        .selected_disk()
        .map(|disk| disk.name.as_str())
        .unwrap_or("?");
    frame.render_widget(
        Paragraph::new(format!(
            "Verified plan for /dev/{target}. No command has run and no data has changed.\nThe previous simulated success screen has been replaced by this explicit review."
        ))
        .block(Block::default().borders(Borders::ALL).title("Plan ready"))
        .wrap(Wrap { trim: false }),
        rows[0],
    );
    let logs: Vec<Line> = app
        .logs
        .iter()
        .rev()
        .take(26)
        .rev()
        .map(|line| Line::from(sanitize_display(line, 512)))
        .collect();
    frame.render_widget(
        Paragraph::new(logs)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Verified details"),
            )
            .wrap(Wrap { trim: false }),
        rows[1],
    );
}

fn render_popup(frame: &mut Frame, title: &str, text: &str, color: Color) {
    let area = centered_rect(frame.area(), 72, 30);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(sanitize_display(text, 500))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(color))
                    .title(title),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn help_text(step: Step) -> String {
    let context = match step {
        Step::PickDisk => "Use r after attaching a disk or changing VMware hardware.",
        Step::Options => "Only safe hostname and absolute content-list values are accepted.",
        Step::Confirm => "Typing the target protects against an accidental selection.",
        Step::Plan => "This screen is a review, not a completed installation.",
    };
    format!(
        "Safety-first Labdoo preflight\n\n\
• The boot disk is always refused.\n\
• VMware ISO boot via /dev/sr0 is recognised as optical media.\n\
• Read-only disks are refused; removable disks are highlighted.\n\
• If the boot medium cannot be identified, target selection stops.\n\
• This build never writes to a disk.\n\n\
{context}\n\nPress any key to close."
    )
}

fn centered_rect(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height_percent) / 2),
            Constraint::Percentage(height_percent),
            Constraint::Percentage((100 - height_percent) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_execute_mode() {
        let cli = Cli {
            image: "labdoo".to_string(),
            image_path: PathBuf::from("/tmp/labdoo"),
            dry_run: false,
            execute: true,
        };
        assert!(validate_startup(&cli).is_err());
    }

    #[test]
    fn accepts_a_posix_image_path_on_any_build_host() {
        let cli = Cli {
            image: "labdoo".to_string(),
            image_path: PathBuf::from("/tmp/labdoo"),
            dry_run: true,
            execute: false,
        };
        assert!(validate_startup(&cli).is_ok());
    }

    #[test]
    fn empty_inventory_is_not_replaced_with_guessed_disks() {
        let discovery = Discovery {
            disks: Vec::new(),
            boot_device: None,
            boot_source: None,
            notes: vec!["lsblk unavailable".to_string()],
        };
        let cli = Cli {
            image: "labdoo".to_string(),
            image_path: PathBuf::from("/tmp/labdoo"),
            dry_run: true,
            execute: false,
        };
        let app = App::new(&cli, discovery);
        assert!(app.disks.is_empty());
        assert!(app.selected_disk().is_none());
    }

    #[test]
    fn preserves_a_known_target_on_refresh_selection() {
        let cli = Cli {
            image: "labdoo".to_string(),
            image_path: PathBuf::from("/tmp/labdoo"),
            dry_run: true,
            execute: false,
        };
        let discovery = Discovery {
            disks: vec![
                DiskInfo {
                    name: "sda".to_string(),
                    size: "1G".to_string(),
                    model: String::new(),
                    transport: String::new(),
                    removable: false,
                    read_only: false,
                },
                DiskInfo {
                    name: "sdb".to_string(),
                    size: "1G".to_string(),
                    model: String::new(),
                    transport: String::new(),
                    removable: false,
                    read_only: false,
                },
            ],
            boot_device: Some(BootDevice::Optical("sr0".to_string())),
            boot_source: Some("/dev/sr0".to_string()),
            notes: Vec::new(),
        };
        let mut app = App::new(&cli, discovery);
        app.select_preferred_disk(Some("sdb"));
        assert_eq!(
            app.selected_disk().map(|disk| disk.name.as_str()),
            Some("sdb")
        );
    }
}
