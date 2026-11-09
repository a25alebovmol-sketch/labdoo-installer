# Labdoo Installer (Ratatui TUI)

Rust + [Ratatui](https://ratatui.rs) port of `exec.sh` — the Labdoo/Labtix
Clonezilla installer that lived inside zzzFM/yad.

The original shell flow is preserved:

1. Validate the Clonezilla image folder (`clonezilla-img` marker).
2. List target disks via `lsblk`, refuse the boot device (`/live/config/boot-device`).
3. Ask for shred passes (`0..3`), hostname, content-list file, poweroff flag.
4. Show the info section from the content list.
5. `shred` → `ocs-sr restoredisk` → mount `/dev/<disk><2|3>` (with `p` prefix
   for `nvme*`/`mmcblk*`) → set hostname → unpack wikis / rsync kiwix /
   set Firefox homepages → fix perms → fresh `machine-id` → unmount.
6. Report missing/installed contents, then poweroff or open `gparted`.

## Run

```bash
cargo run -- --image labdoo-24.04 --image-path /media/usb/images/labdoo-24.04/files
```

Flags:

- `--dry-run` (default `true`): simulate, log the exact `sudo …` commands.
- `--execute`: actually run destructive commands. Linux + root + Clonezilla only.

Keys: `↑/↓` select disk · `Enter` next · `Tab` move field · `←/→` shred level ·
type to edit · `Space` toggles poweroff · `Esc` back · `Ctrl-C` / `q` quit.

## Layout

- `src/installer.rs` — pure, OS-independent logic (`cargo test` covers it).
- `src/main.rs` — Ratatui + crossterm TUI, disk discovery, progress + log.
- `exec.sh` — legacy yad implementation, kept as reference.

## CI

`.github/workflows/ci.yml` runs on every push/PR to `main`:

`cargo fmt --check` → `cargo clippy -- -D warnings` → `cargo test` → `cargo build --release`.

## Safety

Destructive steps (`shred`, `ocs-sr`, `mount`, `chown`) never run unless you pass
`--execute`. Review the Confirm screen / dry-run log first.
