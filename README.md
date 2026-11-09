# Labdoo Installer 0.2

This is a safer, inspection-focused refresh of the Ratatui Labdoo/Labtix
installer. It is deliberately a **preflight release**: it validates the
environment and produces an exact plan, but never erases or clones a disk.

That behaviour is intentional. The previous interface simulated progress when
given the execute option and then displayed a successful installation even
though no destructive command had been implemented. This release refuses that
option instead of making that claim.

## What changed

- Recognises a VMware ISO mounted as /dev/sr0 as known optical boot media.
  That lets /dev/sda remain a valid VM target while still preventing a booted
  USB/disk from being selected.
- Checks the Labtix marker, several live-media mountpoints, and the normal
  root mount before deciding that the boot device is unknown.
- Does not invent target disks if lsblk fails.
- Uses lsblk metadata to show disk name, size, model, transport, removable
  status, and read-only status in the picker.
- Refuses read-only targets and highlights removable media.
- Keeps one target-selection safety gate for both the picker and confirmation
  screen.
- Supports refresh with r, which is useful after changing VMware hardware.
- Requires the target name to be typed exactly before showing the plan.
- Validates absolute content-list paths and stricter hostnames.
- Correctly handles content-list paths that contain spaces.
- Replaces the fake progress/success screen with an explicit reviewed plan.
- Adds unit tests for VMware optical boot, metadata parsing, boot protection,
  read-only disks, paths, command construction, and input validation.

## Build and test

    cargo fmt --check
    cargo test
    cargo clippy -- -D warnings
    cargo build --release

## Run

    cargo run -- --image labdoo-24.04 --image-path /media/usb/images/labdoo-24.04/files

Keys:

- Up/Down: choose a target disk
- r: refresh block-device and boot-media detection
- Tab: switch option fields
- Left/Right: change shred passes
- Space: toggle the future power-off preference
- ? or h: show safety help
- Esc: go back
- q or Ctrl-C: quit

## VMware behaviour

With the common VMware layout:

| Device | Meaning |
| --- | --- |
| /dev/sr0 | ISO/CD used to boot Labtix |
| /dev/sda | Virtual hard disk intended for the image |

The picker will show sr0 (optical media) as the known boot source and allow
sda to proceed through preflight. It never treats sr0 as a target disk.

## Next implementation boundary

An eventual real executor should be added only after a separate review of
mount isolation, safe writes inside the mounted image, content-archive
validation, cleanup on interruption, and reliable command-result reporting.
It should run every external command as an argument vector rather than through
a shell, and it must only report success after every stage has completed.
