//! Pure installer logic ported from `exec.sh`.
//!
//! Everything here is std-only on purpose so `cargo test` runs anywhere
//! (including Windows and GitHub CI) without disks, sudo or Clonezilla.
//! The TUI (`main.rs`) calls into these helpers.

use std::path::{Path, PathBuf};

/// Default hostname meaning "keep the image hostname".
pub const KEEP_HOSTNAME: &str = "labdoo-000xxxxx";

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

/// Strip the last two path components (`.../<image>/<files>` -> `...`).
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

/// Filter raw `lsblk -d -n -o NAME` output the way exec.sh does.
///
/// exec.sh runs `sed 's/sc./ /g;s/sr./ /g;s/loop./ /g'`, i.e. it drops
/// optical/loop-ish devices while keeping `sd*`, `nvme*`, `mmcblk*`, `vd*`, ...
pub fn filter_disks(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .filter(|name| {
            !(name.starts_with("sr") || name.starts_with("sc") || name.starts_with("loop"))
        })
        .map(str::to_string)
        .collect()
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

/// Hostname is valid when non-empty and made of `[A-Za-z0-9-]`.
/// The sentinel `labdoo-000xxxxx` means "do not change".
pub fn validate_hostname(hostname: &str) -> bool {
    let h = hostname.trim();
    !h.is_empty() && h.len() <= 64 && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Extract a `####header` section like exec.sh does with
/// `sed -n "/####lang_1/,/^$/p" | tail -n +2`.
///
/// Returns trimmed, non-empty lines; stops at the first blank line.
pub fn parse_section(content: &str, header: &str) -> Vec<String> {
    let mut lines = content.lines();
    // Find the header line.
    for line in lines.by_ref() {
        if line.trim() == header {
            break;
        }
    }
    let mut out = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            break;
        }
        out.push(line.trim().to_string());
    }
    out
}

/// `sudo shred /dev/<disk> -v -f -n <level>` argv.
pub fn shred_argv(disk: &str, level: u8) -> Vec<String> {
    vec![
        "sudo".to_string(),
        "shred".to_string(),
        format!("/dev/{disk}"),
        "-v".to_string(),
        "-f".to_string(),
        "-n".to_string(),
        level.min(3).to_string(),
    ]
}

/// `sudo /usr/sbin/ocs-sr ... restoredisk <image> <disk>` argv.
pub fn ocs_restore_argv(image: &str, disk: &str) -> Vec<String> {
    vec![
        "sudo".to_string(),
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
pub fn build_plan(image: &str, disk: &str, shred_level: u8, hostname: &str) -> Vec<String> {
    let part = target_partition(disk, 3);
    vec![
        format!("shred: {}", shred_argv(disk, shred_level).join(" ")),
        format!("clone: {}", ocs_restore_argv(image, disk).join(" ")),
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
        let raw = "sda sdb sr0 scd0 loop0 nvme0n1 mmcblk0 vda";
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
    fn builds_expected_commands() {
        assert_eq!(
            shred_argv("sda", 3).join(" "),
            "sudo shred /dev/sda -v -f -n 3"
        );
        let ocs = ocs_restore_argv("labdoo-img", "sda").join(" ");
        assert!(ocs.contains("restoredisk labdoo-img sda"));
        let plan = build_plan("labdoo-img", "sda", 1, KEEP_HOSTNAME);
        assert_eq!(plan.len(), 6);
    }
}
