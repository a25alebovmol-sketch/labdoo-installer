//! Pure, side-effect-free helpers for the Labdoo installer.
//!
//! Keeping parsing, validation and safety decisions here makes them testable
//! without a live system, root access, disks, or Clonezilla.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Default hostname meaning "leave the hostname in the image unchanged".
pub const KEEP_HOSTNAME: &str = "labdoo-000xxxxx";
pub const MAX_IMAGE_LEN: usize = 64;
pub const MAX_HOSTNAME_LEN: usize = 63;
pub const MAX_PATH_LEN: usize = 256;

/// The boot medium discovered by the installer.
///
/// Optical media is deliberately a separate variant: it proves that the live
/// source is known, but can never be selected as a target disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootDevice {
    Disk(String),
    Optical(String),
}

impl BootDevice {
    pub fn description(&self) -> String {
        match self {
            Self::Disk(name) => format!("{name} (disk)"),
            Self::Optical(name) => format!("{name} (optical media)"),
        }
    }

    pub fn blocks_target(&self, target: &str) -> bool {
        matches!(self, Self::Disk(name) if name == target)
    }
}

/// A target-capable block device reported by lsblk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    pub name: String,
    pub size: String,
    pub model: String,
    pub transport: String,
    pub removable: bool,
    pub read_only: bool,
}

impl DiskInfo {
    pub fn label(&self) -> String {
        let mut details = Vec::new();
        if !self.size.is_empty() {
            details.push(self.size.clone());
        }
        if !self.model.is_empty() {
            details.push(self.model.clone());
        }
        if !self.transport.is_empty() {
            details.push(self.transport.clone());
        }
        if self.removable {
            details.push("removable".to_string());
        }
        if self.read_only {
            details.push("READ-ONLY".to_string());
        }
        if details.is_empty() {
            self.name.clone()
        } else {
            format!("{}  —  {}", self.name, details.join(" · "))
        }
    }
}

/// Filesystem locations derived from a Clonezilla image directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DerivedPaths {
    pub root: PathBuf,
    pub content_lists: PathBuf,
    pub content_wikis: PathBuf,
    pub content_kiwix: PathBuf,
}

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

pub fn clonezilla_marker_path(image_path: &Path) -> PathBuf {
    image_path.join("clonezilla-img")
}

/// Parse the stable, shell-friendly lsblk property format.
///
/// Only TYPE="disk" rows make it into the result. This avoids showing
/// optical, loop, mapper, partition, and other non-target devices.
pub fn parse_disk_inventory(raw: &str) -> Vec<DiskInfo> {
    let mut disks = Vec::new();
    for line in raw.lines() {
        let fields = parse_lsblk_properties(line);
        if fields.get("TYPE").map(String::as_str) != Some("disk") {
            continue;
        }
        let Some(name) = fields.get("NAME").cloned() else {
            continue;
        };
        if !is_valid_disk_name(&name) || disks.iter().any(|disk: &DiskInfo| disk.name == name) {
            continue;
        }
        disks.push(DiskInfo {
            name,
            size: fields.get("SIZE").cloned().unwrap_or_default(),
            model: fields.get("MODEL").cloned().unwrap_or_default(),
            transport: fields.get("TRAN").cloned().unwrap_or_default(),
            removable: fields.get("RM").map(String::as_str) == Some("1"),
            read_only: fields.get("RO").map(String::as_str) == Some("1"),
        });
    }
    disks
}

fn parse_lsblk_properties(line: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut chars = line.chars().peekable();

    while chars.peek().is_some() {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let mut key = String::new();
        while let Some(character) = chars.peek().copied() {
            if character == '=' || character.is_whitespace() {
                break;
            }
            key.push(character);
            chars.next();
        }
        if key.is_empty() || chars.next() != Some('=') {
            break;
        }

        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            let mut escaped = false;
            for c in chars.by_ref() {
                if escaped {
                    value.push(c);
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    break;
                } else {
                    value.push(c);
                }
            }
        } else {
            value.extend(chars.by_ref().take_while(|c| !c.is_whitespace()));
        }
        fields.insert(key, value);
    }
    fields
}

/// Strict allow-list for disk base names (no partitions).
pub fn is_valid_disk_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 32 || name.starts_with('-') || name.contains('/') {
        return false;
    }
    if let Some(rest) = name.strip_prefix("nvme") {
        let mut parts = rest.splitn(2, 'n');
        let (Some(controller), Some(namespace)) = (parts.next(), parts.next()) else {
            return false;
        };
        return !controller.is_empty()
            && !namespace.is_empty()
            && controller.chars().all(|c| c.is_ascii_digit())
            && namespace.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = name.strip_prefix("mmcblk") {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    for prefix in ["xvd", "sd", "vd", "hd"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return (1..=2).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_lowercase());
        }
    }
    false
}

pub fn is_valid_image_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_IMAGE_LEN
        && !name.starts_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Content list paths are absolute, non-traversing paths with no control or
/// shell-significant characters. Spaces are intentionally allowed.
pub fn is_valid_content_list_value(value: &str) -> bool {
    value.starts_with('/')
        && !value.is_empty()
        && value.len() <= MAX_PATH_LEN
        && !value.contains('\0')
        && !value.chars().any(|c| c.is_control())
        && value.split('/').all(|part| part != "..")
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ' '))
}

pub fn sanitize_display(input: &str, max: usize) -> String {
    input
        .chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Derive the base disk from a conventional Linux block-device source.
pub fn disk_from_source(src: &str) -> Option<String> {
    let rest = src.trim().strip_prefix("/dev/")?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    if let Some(after) = rest.strip_prefix("nvme") {
        let (base, part_ok) = match after.split_once('p') {
            Some((base, suffix)) => (
                format!("nvme{base}"),
                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()),
            ),
            None => (format!("nvme{after}"), true),
        };
        return (part_ok && is_valid_disk_name(&base)).then_some(base);
    }
    if let Some(after) = rest.strip_prefix("mmcblk") {
        let base = match after.split_once('p') {
            Some((digits, suffix))
                if !digits.is_empty()
                    && !suffix.is_empty()
                    && suffix.chars().all(|c| c.is_ascii_digit()) =>
            {
                format!("mmcblk{digits}")
            }
            Some(_) => return None,
            None => format!("mmcblk{after}"),
        };
        return is_valid_disk_name(&base).then_some(base);
    }
    for prefix in ["xvd", "sd", "vd", "hd"] {
        if let Some(after) = rest.strip_prefix(prefix) {
            let letters: String = after
                .chars()
                .take_while(|c| c.is_ascii_lowercase())
                .collect();
            let suffix = &after[letters.len()..];
            if (1..=2).contains(&letters.len())
                && (suffix.is_empty() || suffix.chars().all(|c| c.is_ascii_digit()))
            {
                let base = format!("{prefix}{letters}");
                if is_valid_disk_name(&base) {
                    return Some(base);
                }
            }
        }
    }
    None
}

pub fn optical_device_from_source(source: &str) -> Option<String> {
    let device = source.trim().strip_prefix("/dev/")?;
    let number = device
        .strip_prefix("sr")
        .or_else(|| device.strip_prefix("scd"))?;
    (!number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then(|| device.to_string())
}

pub fn boot_device_from_source(source: &str) -> Option<BootDevice> {
    disk_from_source(source)
        .map(BootDevice::Disk)
        .or_else(|| optical_device_from_source(source).map(BootDevice::Optical))
}

/// A single target-selection gate for both the picker and confirm screen.
/// Unknown boot media always blocks the operation.
pub fn target_disk_error(
    boot_device: Option<&BootDevice>,
    target: Option<&DiskInfo>,
) -> Option<String> {
    let Some(target) = target else {
        return Some("no target disk selected".to_string());
    };
    if target.read_only {
        return Some(format!("refusing: {} is read-only", target.name));
    }
    let Some(boot_device) = boot_device else {
        return Some("refusing: boot device unknown (fail-closed)".to_string());
    };
    if boot_device.blocks_target(&target.name) {
        return Some(format!("refusing: {} is the boot device", target.name));
    }
    None
}

pub fn partition_suffix(disk: &str, part_number: u8) -> String {
    if disk.starts_with("nvme")
        || disk.starts_with("mmcblk")
        || disk.chars().last().is_some_and(|c| c.is_ascii_digit())
    {
        format!("p{part_number}")
    } else {
        part_number.to_string()
    }
}

pub fn target_partition(disk: &str, part_number: u8) -> String {
    format!("{disk}{}", partition_suffix(disk, part_number))
}

pub fn resolve_part_number(has_partition3: bool) -> u8 {
    if has_partition3 {
        3
    } else {
        2
    }
}

pub fn validate_hostname(hostname: &str) -> bool {
    let hostname = hostname.trim();
    hostname == KEEP_HOSTNAME
        || (!hostname.is_empty()
            && hostname.len() <= MAX_HOSTNAME_LEN
            && !hostname.starts_with('-')
            && !hostname.ends_with('-')
            && hostname
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

pub fn parse_section(content: &str, header: &str) -> Vec<String> {
    let mut lines = content.lines();
    if !lines.by_ref().any(|line| line.trim() == header) {
        return Vec::new();
    }
    lines
        .take_while(|line| !line.trim().is_empty())
        .take(500)
        .map(|line| line.trim().chars().take(512).collect())
        .collect()
}

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

pub fn build_plan(image: &str, disk: &str, shred_level: u8, hostname: &str) -> Vec<String> {
    let partition = target_partition(disk, resolve_part_number(true));
    let image = sanitize_display(image, MAX_IMAGE_LEN);
    let disk = sanitize_display(disk, 32);
    let hostname = sanitize_display(hostname, MAX_HOSTNAME_LEN);
    vec![
        format!("shred: {}", shred_argv(&disk, shred_level).join(" ")),
        format!("clone: {}", ocs_restore_argv(&image, &disk).join(" ")),
        format!("mount /dev/{partition} on a private mountpoint"),
        if hostname == KEEP_HOSTNAME {
            "hostname: keep image default".to_string()
        } else {
            format!("hostname: set to {hostname}")
        },
        "contents: validate and install requested archives".to_string(),
        "finish: fix permissions, regenerate machine-id, unmount".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(name: &str) -> DiskInfo {
        DiskInfo {
            name: name.to_string(),
            size: "30G".to_string(),
            model: "VMware Virtual S".to_string(),
            transport: "spi".to_string(),
            removable: false,
            read_only: false,
        }
    }

    #[test]
    fn parses_lsblk_inventory_with_models_and_filters_non_disks() {
        let raw = concat!(
            "NAME=\"sda\" SIZE=\"30G\" MODEL=\"VMware Virtual S\" TRAN=\"spi\" RM=\"0\" RO=\"0\" TYPE=\"disk\"\n",
            "NAME=\"sr0\" SIZE=\"1.2G\" MODEL=\"VMware SATA CD\" TRAN=\"sata\" RM=\"1\" RO=\"1\" TYPE=\"rom\"\n",
            "NAME=\"loop0\" SIZE=\"1G\" MODEL=\"\" TRAN=\"\" RM=\"0\" RO=\"1\" TYPE=\"loop\"\n",
        );
        assert_eq!(parse_disk_inventory(raw), vec![disk("sda")]);
    }

    #[test]
    fn parses_escaped_lsblk_values() {
        let raw = "NAME=\"sda\" SIZE=\"1G\" MODEL=\"Disk with \\\"quotes\\\"\" TRAN=\"\" RM=\"0\" RO=\"0\" TYPE=\"disk\"";
        assert_eq!(parse_disk_inventory(raw)[0].model, "Disk with \"quotes\"");
    }

    #[test]
    fn allow_lists_only_plausible_disk_names() {
        for name in ["sda", "sdaa", "vda", "xvda", "nvme0n1", "mmcblk0"] {
            assert!(is_valid_disk_name(name), "{name}");
        }
        for name in ["", "--help", "sda;rm", "../sda", "sr0", "sda1"] {
            assert!(!is_valid_disk_name(name), "{name}");
        }
    }

    #[test]
    fn parses_kernel_sources_without_guessing() {
        assert_eq!(disk_from_source("/dev/sda1"), Some("sda".to_string()));
        assert_eq!(
            disk_from_source("/dev/nvme0n1p2"),
            Some("nvme0n1".to_string())
        );
        assert_eq!(
            disk_from_source("/dev/mmcblk0p1"),
            Some("mmcblk0".to_string())
        );
        assert_eq!(disk_from_source("/dev/mapper/vg-root"), None);
        assert_eq!(disk_from_source("/dev/sda;rm"), None);
    }

    #[test]
    fn identifies_vmware_optical_boot_media() {
        assert_eq!(
            boot_device_from_source("/dev/sr0"),
            Some(BootDevice::Optical("sr0".to_string()))
        );
        assert_eq!(
            boot_device_from_source("/dev/sda1"),
            Some(BootDevice::Disk("sda".to_string()))
        );
    }

    #[test]
    fn optical_boot_media_allows_a_disk_but_disk_boot_does_not() {
        let sda = disk("sda");
        assert_eq!(
            target_disk_error(Some(&BootDevice::Optical("sr0".to_string())), Some(&sda)),
            None
        );
        assert!(
            target_disk_error(Some(&BootDevice::Disk("sda".to_string())), Some(&sda))
                .unwrap()
                .contains("boot device")
        );
        assert!(target_disk_error(None, Some(&sda))
            .unwrap()
            .contains("unknown"));
    }

    #[test]
    fn refuses_read_only_targets() {
        let mut sda = disk("sda");
        sda.read_only = true;
        assert!(
            target_disk_error(Some(&BootDevice::Optical("sr0".to_string())), Some(&sda))
                .unwrap()
                .contains("read-only")
        );
    }

    #[test]
    fn validates_image_and_content_list_values() {
        assert!(is_valid_image_name("labdoo-24.04"));
        assert!(!is_valid_image_name("--help"));
        assert!(is_valid_content_list_value(
            "/media/usb/wiki archive/content_list/none"
        ));
        assert!(!is_valid_content_list_value("relative/list"));
        assert!(!is_valid_content_list_value("/etc/../shadow"));
    }

    #[test]
    fn validates_hostnames() {
        assert!(validate_hostname(KEEP_HOSTNAME));
        assert!(validate_hostname("labdoo-000123"));
        assert!(!validate_hostname("-bad"));
        assert!(!validate_hostname("bad-"));
        assert!(!validate_hostname("bad name"));
    }

    #[test]
    fn derives_paths_like_the_legacy_script() {
        let paths = derive_paths(Path::new("/media/usb/images/labdoo-24.04/files"));
        assert_eq!(paths.root, PathBuf::from("/media/usb/images"));
        assert_eq!(
            paths.content_lists,
            PathBuf::from("/media/usb/images/wiki-archive/content_list")
        );
    }

    #[test]
    fn partitions_and_plans_are_safe_and_predictable() {
        assert_eq!(target_partition("sda", 3), "sda3");
        assert_eq!(target_partition("nvme0n1", 3), "nvme0n1p3");
        assert_eq!(resolve_part_number(true), 3);
        assert_eq!(resolve_part_number(false), 2);
        let plan = build_plan("labdoo", "sda", 7, KEEP_HOSTNAME);
        assert!(plan[0].ends_with("-n 3"));
        assert_eq!(plan.len(), 6);
    }

    #[test]
    fn strips_control_characters_from_ui_text() {
        let value = sanitize_display("\u{1b}[2J sda\n", 32);
        assert!(!value.contains('\u{1b}'));
        assert!(!value.contains('\n'));
    }

    #[test]
    fn parses_content_sections_with_a_cap() {
        let content = "####info\none\ntwo\n\n####lang_1\nen\n";
        assert_eq!(parse_section(content, "####info"), vec!["one", "two"]);
        assert!(parse_section(content, "####missing").is_empty());
    }
}
