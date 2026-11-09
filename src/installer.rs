//! Pure installer logic ported from `exec.sh`.
//!
//! Everything here is std-only on purpose so `cargo test` runs anywhere
//! (including Windows and GitHub CI) without disks, sudo or Clonezilla.
//! The TUI (`main.rs`) calls into these helpers.
//!
//! Security notes:
//! - Disk and image names are allow-listed (no shell metachars, no `--flags`).
//! - Display strings are sanitised (no ANSI escapes / control chars in the UI).
//! - No shell is ever used: callers must spawn `Command` with these argvs.

use std::path::{Path, PathBuf};

/// Default hostname meaning "keep the image hostname".
pub const KEEP_HOSTNAME: &str = "labdoo-000xxxxx";

/// Max lengths enforced both in pure logic and in the TUI key handler.
pub const MAX_IMAGE_LEN: usize = 64;
pub const MAX_HOSTNAME_LEN: usize = 64;
pub const MAX_PATH_LEN: usize = 256;

/// Derived filesystem locations for a given Clonezilla image directory.
///
/// Mirrors exec.sh:
/// ```sh
/// root_path=$(echo $image_path | rev | cut -d "/" -f 3- | rev)
/// content_lists_path="$root_path/wiki-archive/content_list"
/// content_wikis_path="$root_path/wiki-archive/wikis/"
/// content_kiwix_path="$root_path/wiki-archive/kiwix/wikis/"
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedPaths {
    pub root: PathBuf,
    pub content_lists: PathBuf,
    pub content_wikis: PathBuf,
    pub content_kiwix: PathBuf,
}

/// Strip the last two path components (`.../<image>` -> `...`).
pub fn derive_paths(image_path: &Path) -> DerivedPaths {
    let root = image_path
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    DerivedPaths {
        content_lists: root.join("wiki-archive/content_list"),
        content_wikis: root.join("wiki-archive/wikis"),
        content_kiwix: root.join("wiki-archive/kiwix/wikis"),
        root,
    }
}

/// Path of the Clonezilla marker file exec.sh checks (`clonezilla-img`).
pub fn clonezilla_marker_path(image_path: &Path) -> PathBuf {
    image_path.join("clonezilla-img")
}

/// Filter raw `lsblk -d -n -o NAME` output the way exec.sh does,
/// plus an allow-list so unexpected / malicious names never reach the UI.
///
/// Drops `sr*`, `sc*`, `loop*` and anything that is not a plausible
/// `sd/vd/hd/nvme/mmcblk` disk name.
pub fn filter_disks(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .filter(|name| {
            !(name.starts_with("sr") || name.starts_with("sc") || name.starts_with("loop"))
        })
        .filter(|name| is_valid_disk_name(name))
        .map(str::to_string)
        .collect()
}

/// Strict allow-list for disk base names (no partition suffix).
///
/// Accepted: `sd[a-z]{1,2}`, `vd[a-z]{1,2}`, `hd[a-z]{1,2}`, `xvd[a-z]{1,2}`,
/// `nvme<digits>n<digits>`, `mmcblk<digits>`.
pub fn is_valid_disk_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 {
        return false;
    }
    if name.starts_with('-') || name.contains('/') || name.contains('\0') {
        return false;
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        // e.g. nvme0n1
        let mut parts = rest.splitn(2, 'n');
        let (Some(a), Some(b)) = (parts.next(), parts.next()) else {
            return false;
        };
        return !a.is_empty()
            && !b.is_empty()
            && a.chars().all(|c| c.is_ascii_digit())
            && b.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    for prefix in ["sd", "vd", "hd", "xvd"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return (1..=2).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_lowercase());
        }
    }
    false
}

/// Image names become argv to `ocs-sr`; forbid option-injection and shell chars.
pub fn is_valid_image_name(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_IMAGE_LEN {
        return false;
    }
    if name.starts_with('-') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Content-list text field: absolute path chars only, no `..`, no shell metachars.
pub fn is_valid_content_list_value(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_PATH_LEN {
        return false;
    }
    if value.contains('\0') || value.chars().any(|c| c.is_control()) {
        return false;
    }
    if value.contains("..") {
        return false;
    }
    value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ' '))
}

/// Strip control chars / ANSI escapes for safe TUI display, truncate to `max`.
pub fn sanitize_display(input: &str, max: usize) -> String {
    let clean: String = input
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect();
    clean.trim().to_string()
}

/// Derive the disk base name from a kernel source path.
///
/// Handles `/dev/sda1` -> `sda`, `/dev/nvme0n1p2` -> `nvme0n1`,
/// `/dev/mmcblk0p1` -> `mmcblk0`. Returns `None` for LVM/dm/unknown so
/// callers fail closed instead of guessing the boot disk wrong.
pub fn disk_from_source(src: &str) -> Option<String> {
    let dev = src.trim();
    let rest = dev.strip_prefix("/dev/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    // NVMe: nvme<n>n<m>[p<part>]
    if let Some(after) = rest.strip_prefix("nvme") {
        let (base_part, part_ok) = match after.split_once('p') {
            Some((base, suffix)) => (
                format!("nvme{base}"),
                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()),
            ),
            None => (format!("nvme{after}"), true),
        };
        if !part_ok || !is_valid_disk_name(&base_part) {
            return None;
        }
        return Some(base_part);
    }
    // MMC: mmcblk<n>[p<part>]
    if let Some(after) = rest.strip_prefix("mmcblk") {
        let base = match after.split_once('p') {
            Some((digits, suffix)) => {
                if suffix.is_empty() || !suffix.chars().all(|c| c.is_ascii_digit()) {
                    return None;
                }
                format!("mmcblk{digits}")
            }
            None => format!("mmcblk{after}"),
        };
        if !is_valid_disk_name(&base) {
            return None;
        }
        return Some(base);
    }
    // sd/vd/hd/xvd + letters + optional partition digits, e.g. sda, sda1, vda12.
    for prefix in ["xvd", "sd", "vd", "hd"] {
        if let Some(after) = rest.strip_prefix(prefix) {
            let letters: String = after
                .chars()
                .take_while(|c| c.is_ascii_lowercase())
                .collect();
            let digits: String = after[letters.len()..].chars().collect();
            if !(1..=2).contains(&letters.len()) {
                continue;
            }
            if !digits.is_empty() && !digits.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let base = format!("{prefix}{letters}");
            if is_valid_disk_name(&base) {
                return Some(base);
            }
        }
    }
    None
}

/// True when the user picked the device the live system booted from.
pub fn is_boot_disk_selected(boot_device: &str, selected: &str) -> bool {
    !boot_device.is_empty() && boot_device.trim() == selected.trim()
}

/// Partition suffix for a disk + partition number.
///
/// SATA (`sda` + 3 -> `3`), NVMe/MMC or any disk ending in a digit
/// (`nvme0n1` + 3 -> `p3`, `mmcblk0` + 3 -> `p3`).
pub fn partition_suffix(disk: &str, part_number: u8) -> String {
    let needs_p = disk.starts_with("nvme")
        || disk.starts_with("mmcblk")
        || disk.chars().last().is_some_and(|c| c.is_ascii_digit());
    if needs_p {
        format!("p{part_number}")
    } else {
        part_number.to_string()
    }
}

/// Full partition device name without `/dev/` prefix, e.g. `sda3`, `nvme0n1p3`.
pub fn target_partition(disk: &str, part_number: u8) -> String {
    format!("{disk}{}", partition_suffix(disk, part_number))
}

/// exec.sh probes for partition 3 and falls back to partition 2
/// (two- vs three-partition Labtix images).
pub fn resolve_part_number(has_partition3: bool) -> u8 {
    if has_partition3 {
        3
    } else {
        2
    }
}

/// Hostname is valid when non-empty and made of `[A-Za-z0-9-]` (max 64).
/// The sentinel `labdoo-000xxxxx` means "do not change".
pub fn validate_hostname(hostname: &str) -> bool {
    let h = hostname.trim();
    !h.is_empty()
        && h.len() <= MAX_HOSTNAME_LEN
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Extract a `####header` section like exec.sh does with
/// `sed -n "/####lang_1/,/^$/p" | tail -n +2`.
///
/// Returns trimmed lines; stops at the first blank line; caps at 500 lines
/// so a hostile content list cannot blow up memory.
pub fn parse_section(content: &str, header: &str) -> Vec<String> {
    let mut lines = content.lines();
    let mut found = false;
    for line in lines.by_ref() {
        if line.trim() == header {
            found = true;
            break;
        }
    }
    if !found {
        return Vec::new();
    }
    let mut out = Vec::new();
    for line in lines.take(500) {
        if line.trim().is_empty() {
            break;
        }
        // Cap line length to avoid giant allocations from a hostile file.
        out.push(line.trim().chars().take(512).collect());
    }
    out
}

/// `shred /dev/<disk> -v -f -n <level>` argv (no shell, no `sudo` prefix here;
/// the caller decides privilege escalation).
pub fn shred_argv(disk: &str, level: u8) -> Vec<String> {
    vec![
        "shred".to_string(),
        format!("/dev/{disk}"),
        "-v".to_string(),
        "-f".to_string(),
        "-n".to_string(),
        level.min(3).to_string(),
    ]
}

/// `/usr/sbin/ocs-sr ... restoredisk <image> <disk>` argv (no shell).
pub fn ocs_restore_argv(image: &str, disk: &str) -> Vec<String> {
    vec![
        "/usr/sbin/ocs-sr".to_string(),
        "-e1".to_string(),
        "auto".to_string(),
        "-e2".to_string(),
        "-r".to_string(),
        "-icrc".to_string(),
        "-icds".to_string(),
        "-scr".to_string(),
        "-j2".to_string(),
        "-k1".to_string(),
        "-p".to_string(),
        "true".to_string(),
        "restoredisk".to_string(),
        image.to_string(),
        disk.to_string(),
    ]
}

/// Human-readable install plan (shown in Confirm step and `--dry-run` logs).
/// All interpolated values are display-sanitised.
pub fn build_plan(image: &str, disk: &str, shred_level: u8, hostname: &str) -> Vec<String> {
    let part = target_partition(disk, 3);
    let image = sanitize_display(image, MAX_IMAGE_LEN);
    let disk = sanitize_display(disk, 32);
    let hostname = sanitize_display(hostname, MAX_HOSTNAME_LEN);
    vec![
        format!("shred: {}", shred_argv(&disk, shred_level).join(" ")),
        format!("clone: {}", ocs_restore_argv(&image, &disk).join(" ")),
        format!("mount /dev/{part} on /mnt"),
        if hostname.trim() == KEEP_HOSTNAME {
            "hostname: keep image default".to_string()
        } else {
            format!("hostname: set to {hostname}")
        },
        "contents: unpack wikis / rsync kiwix / set firefox homepages".to_string(),
        "finish: fix perms, machine-id, unmount, poweroff-or-gparted".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_optical_and_loop_devices() {
        let raw = "sda sdb sr0 scd0 loop0 nvme0n1 mmcblk0 vda --help";
        assert_eq!(
            filter_disks(raw),
            vec!["sda", "sdb", "nvme0n1", "mmcblk0", "vda"]
        );
    }

    #[test]
    fn filters_empty_input() {
        assert!(filter_disks("   \n ").is_empty());
    }

    #[test]
    fn allow_lists_disk_names() {
        assert!(is_valid_disk_name("sda"));
        assert!(is_valid_disk_name("nvme0n1"));
        assert!(is_valid_disk_name("mmcblk0"));
        assert!(!is_valid_disk_name("--help"));
        assert!(!is_valid_disk_name("sda;rm"));
        assert!(!is_valid_disk_name(""));
        assert!(!is_valid_disk_name("../sda"));
    }

    #[test]
    fn rejects_image_option_injection() {
        assert!(is_valid_image_name("labdoo-24.04"));
        assert!(!is_valid_image_name("--help"));
        assert!(!is_valid_image_name("a;b"));
        assert!(!is_valid_image_name(""));
    }

    #[test]
    fn parses_kernel_sources_without_guessing() {
        assert_eq!(disk_from_source("/dev/sda1"), Some("sda".to_string()));
        assert_eq!(disk_from_source("/dev/sda"), Some("sda".to_string()));
        assert_eq!(
            disk_from_source("/dev/nvme0n1p2"),
            Some("nvme0n1".to_string())
        );
        assert_eq!(
            disk_from_source("/dev/nvme0n1"),
            Some("nvme0n1".to_string())
        );
        assert_eq!(
            disk_from_source("/dev/mmcblk0p1"),
            Some("mmcblk0".to_string())
        );
        assert_eq!(disk_from_source("/dev/mapper/vg-root"), None);
        assert_eq!(disk_from_source("/dev/sda; rm"), None);
        assert_eq!(disk_from_source("sda1"), None);
    }

    #[test]
    fn sanitises_display() {
        // ESC (0x1b) and newline are stripped, neutralising CSI sequences.
        let out = sanitize_display("\x1b[2Jsda\n", 16);
        assert!(!out.contains('\x1b'));
        assert!(!out.contains('\n'));
        assert!(out.contains("sda"));
        assert_eq!(sanitize_display("  sda  ", 16), "sda");
    }

    #[test]
    fn derives_paths_like_exec_sh() {
        let p = derive_paths(Path::new("/media/usb/images/labdoo-24.04"));
        assert_eq!(p.root, PathBuf::from("/media/usb"));
        assert_eq!(
            p.content_lists,
            PathBuf::from("/media/usb/wiki-archive/content_list")
        );
        assert_eq!(
            p.content_wikis,
            PathBuf::from("/media/usb/wiki-archive/wikis")
        );
        assert_eq!(
            p.content_kiwix,
            PathBuf::from("/media/usb/wiki-archive/kiwix/wikis")
        );
    }

    #[test]
    fn partition_names() {
        assert_eq!(target_partition("sda", 3), "sda3");
        assert_eq!(target_partition("sdb", 2), "sdb2");
        assert_eq!(target_partition("nvme0n1", 3), "nvme0n1p3");
        assert_eq!(target_partition("mmcblk0", 3), "mmcblk0p3");
        assert_eq!(target_partition("vda", 2), "vda2");
    }

    #[test]
    fn resolves_part_number() {
        assert_eq!(resolve_part_number(true), 3);
        assert_eq!(resolve_part_number(false), 2);
    }

    #[test]
    fn validates_hostnames() {
        assert!(validate_hostname("labdoo-000123"));
        assert!(validate_hostname(KEEP_HOSTNAME));
        assert!(!validate_hostname(""));
        assert!(!validate_hostname("bad name!"));
        assert!(!validate_hostname("a;b"));
    }

    #[test]
    fn validates_content_lists() {
        assert!(is_valid_content_list_value(
            "/media/usb/wiki-archive/content_list/none"
        ));
        assert!(!is_valid_content_list_value("a; rm -rf /"));
        assert!(!is_valid_content_list_value("/etc/../shadow"));
    }

    #[test]
    fn detects_boot_disk_selection() {
        assert!(is_boot_disk_selected("sda", "sda"));
        assert!(!is_boot_disk_selected("sda", "sdb"));
        assert!(!is_boot_disk_selected("", "sda"));
    }

    #[test]
    fn parses_content_list_sections() {
        let list = "####info\nline a\nline b\n\n####lang_1\nen\n\n####wikis_1\nwiki-en.tar.gz\n\n";
        assert_eq!(parse_section(list, "####info"), vec!["line a", "line b"]);
        assert_eq!(parse_section(list, "####lang_1"), vec!["en"]);
        assert_eq!(parse_section(list, "####wikis_1"), vec!["wiki-en.tar.gz"]);
        assert!(parse_section(list, "####missing").is_empty());
    }

    #[test]
    fn builds_expected_commands_without_shell() {
        assert_eq!(shred_argv("sda", 3).join(" "), "shred /dev/sda -v -f -n 3");
        let ocs = ocs_restore_argv("labdoo-img", "sda").join(" ");
        assert!(ocs.contains("restoredisk labdoo-img sda"));
        assert!(!ocs.contains("sudo"));
        let plan = build_plan("labdoo-img", "sda", 1, KEEP_HOSTNAME);
        assert_eq!(plan.len(), 6);
    }
}
