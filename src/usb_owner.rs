use serde::Serialize;

use crate::output::AppError;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UsbOwnerKind {
    ExclusiveOwner,
    UserClient,
}

impl std::fmt::Display for UsbOwnerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            UsbOwnerKind::ExclusiveOwner => "exclusive-owner",
            UsbOwnerKind::UserClient => "user-client",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UsbOwnerScope {
    Device,
    Interface,
    Other,
}

impl std::fmt::Display for UsbOwnerScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            UsbOwnerScope::Device => "device",
            UsbOwnerScope::Interface => "interface",
            UsbOwnerScope::Other => "other",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UsbOwner {
    pub kind: UsbOwnerKind,
    pub scope: UsbOwnerScope,
    pub scope_node_name: String,
    pub owner_node_name: String,
    pub owner_node_class: String,
    pub interface_number: Option<u8>,
    pub interface_name: Option<String>,
    pub interface_class: Option<u8>,
    pub interface_subclass: Option<u8>,
    pub interface_protocol: Option<u8>,
    pub pid: Option<u32>,
    pub process: String,
    pub raw: String,
}

#[cfg(target_os = "macos")]
pub fn inspect_tp7_usb_owners() -> Result<Vec<UsbOwner>, AppError> {
    let output = std::process::Command::new("ioreg")
        .args(["-r", "-c", "IOUSBHostDevice", "-d", "3", "-l", "-w", "0"])
        .output()
        .map_err(|error| AppError::UsbOwnershipInspection {
            message: error.to_string(),
        })?;

    if !output.status.success() {
        return Err(AppError::UsbOwnershipInspection {
            message: format!("ioreg exited with {}", output.status),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_ioreg_usb_owners(&stdout))
}

#[cfg(not(target_os = "macos"))]
pub fn inspect_tp7_usb_owners() -> Result<Vec<UsbOwner>, AppError> {
    Ok(Vec::new())
}

pub fn is_conflicting_owner(owner: &UsbOwner) -> bool {
    let process = owner.process.to_lowercase();

    process.contains("field kit")
        || process.contains("fieldkit")
        || process.contains("android file transfer")
        || process.contains("openmtp")
        || process.contains("ptpcamerad")
}

pub fn ptpcamerad_exclusive_owner_pids(owners: &[UsbOwner]) -> Vec<u32> {
    let mut pids = owners
        .iter()
        .filter(|owner| {
            owner.kind == UsbOwnerKind::ExclusiveOwner
                && owner.scope == UsbOwnerScope::Interface
                && owner.process.eq_ignore_ascii_case("ptpcamerad")
        })
        .filter_map(|owner| owner.pid)
        .collect::<Vec<_>>();

    pids.sort_unstable();
    pids.dedup();
    pids
}

fn parse_ioreg_usb_owners(output: &str) -> Vec<UsbOwner> {
    let entries = parse_ioreg_entries(output);
    let mut owners = Vec::new();
    let has_tp7_device = entries.iter().any(is_tp7_device_entry);

    for (index, entry) in entries.iter().enumerate() {
        if has_tp7_device && !owner_belongs_to_tp7_device(&entries, index) {
            continue;
        }

        for owner in &entry.owners {
            let scope_index = nearest_scope_entry(&entries, index);
            let scope_entry = &entries[scope_index];
            let (pid, process) = parse_owner(&owner.raw);
            let raw = public_owner_raw(&owner.raw, pid, &process);
            let owner_node_name = public_owner_node_name(&entry.node_name, &process);
            let process = public_owner_process(&process);

            owners.push(UsbOwner {
                kind: owner.kind.clone(),
                scope: scope_for_entry(scope_entry),
                scope_node_name: scope_entry.node_name.clone(),
                owner_node_name,
                owner_node_class: entry.node_class.clone(),
                interface_number: scope_entry.interface_number,
                interface_name: scope_entry.interface_name.clone(),
                interface_class: scope_entry.interface_class,
                interface_subclass: scope_entry.interface_subclass,
                interface_protocol: scope_entry.interface_protocol,
                pid,
                process,
                raw,
            });
        }
    }

    owners
}

#[derive(Debug, Clone, Default)]
struct IoregEntry {
    node_name: String,
    node_class: String,
    parent: Option<usize>,
    interface_number: Option<u8>,
    interface_name: Option<String>,
    interface_class: Option<u8>,
    interface_subclass: Option<u8>,
    interface_protocol: Option<u8>,
    id_vendor: Option<u16>,
    id_product: Option<u16>,
    owners: Vec<ParsedOwner>,
}

#[derive(Debug, Clone)]
struct ParsedOwner {
    kind: UsbOwnerKind,
    raw: String,
}

fn parse_ioreg_entries(output: &str) -> Vec<IoregEntry> {
    let mut entries = Vec::new();
    let mut stack: Vec<usize> = Vec::new();

    for line in output.lines() {
        if let Some((depth, node_name, node_class)) = parse_node_line(line) {
            stack.truncate(depth);
            let parent = depth
                .checked_sub(1)
                .and_then(|index| stack.get(index).copied());
            let index = entries.len();

            entries.push(IoregEntry {
                node_name,
                node_class,
                parent,
                ..IoregEntry::default()
            });
            stack.push(index);
            continue;
        }

        let Some(current_index) = stack.last().copied() else {
            continue;
        };

        let Some((key, value)) = parse_property_line(line) else {
            continue;
        };

        let entry = &mut entries[current_index];
        match key.as_str() {
            "IOUserClientCreator" => entry.owners.push(ParsedOwner {
                kind: UsbOwnerKind::UserClient,
                raw: value,
            }),
            "UsbExclusiveOwner" => entry.owners.push(ParsedOwner {
                kind: UsbOwnerKind::ExclusiveOwner,
                raw: value,
            }),
            "bInterfaceNumber" => entry.interface_number = parse_u8(&value),
            "bInterfaceClass" => entry.interface_class = parse_u8(&value),
            "bInterfaceSubClass" => entry.interface_subclass = parse_u8(&value),
            "bInterfaceProtocol" => entry.interface_protocol = parse_u8(&value),
            "kUSBString" => entry.interface_name = Some(value),
            "idVendor" => entry.id_vendor = parse_u16(&value),
            "idProduct" => entry.id_product = parse_u16(&value),
            _ => {}
        }
    }

    entries
}

fn parse_node_line(line: &str) -> Option<(usize, String, String)> {
    let marker = "+-o ";
    let marker_index = line.find(marker)?;
    let prefix = &line[..marker_index];
    let rest = &line[(marker_index + marker.len())..];
    let class_marker = "  <class ";
    let class_index = rest.find(class_marker)?;
    let node_name = rest[..class_index].trim().to_string();
    let class_rest = &rest[(class_index + class_marker.len())..];
    let node_class = class_rest
        .split([',', '>'])
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();

    Some((prefix.chars().count() / 2, node_name, node_class))
}

fn parse_property_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim_start_matches([' ', '|']);
    let trimmed = trimmed.trim();
    let trimmed = trimmed.strip_prefix('"')?;
    let (key, rest) = trimmed.split_once("\" = ")?;
    let value = rest.trim().trim_end_matches(',').trim();

    Some((key.to_string(), unquote(value)))
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_string()
}

fn parse_u8(value: &str) -> Option<u8> {
    value.parse().ok()
}

fn parse_u16(value: &str) -> Option<u16> {
    value.parse().ok()
}

fn owner_belongs_to_tp7_device(entries: &[IoregEntry], index: usize) -> bool {
    let mut current = Some(index);

    while let Some(entry_index) = current {
        let entry = &entries[entry_index];

        if entry.node_class == "IOUSBHostDevice" {
            return is_tp7_device_entry(entry);
        }

        current = entry.parent;
    }

    false
}

fn is_tp7_device_entry(entry: &IoregEntry) -> bool {
    entry.node_class == "IOUSBHostDevice"
        && entry.id_vendor == Some(crate::device::TP7_VENDOR_ID)
        && entry
            .id_product
            .is_some_and(crate::device::is_tp7_product_id)
}

fn nearest_scope_entry(entries: &[IoregEntry], index: usize) -> usize {
    let mut current = Some(index);

    while let Some(entry_index) = current {
        let entry = &entries[entry_index];

        if matches!(
            entry.node_class.as_str(),
            "IOUSBHostInterface" | "IOUSBHostDevice"
        ) {
            return entry_index;
        }

        current = entry.parent;
    }

    index
}

fn scope_for_entry(entry: &IoregEntry) -> UsbOwnerScope {
    match entry.node_class.as_str() {
        "IOUSBHostDevice" => UsbOwnerScope::Device,
        "IOUSBHostInterface" => UsbOwnerScope::Interface,
        _ => UsbOwnerScope::Other,
    }
}

fn parse_owner(raw: &str) -> (Option<u32>, String) {
    let Some(rest) = raw.strip_prefix("pid ") else {
        return (None, raw.to_string());
    };

    let Some((pid, process)) = rest.split_once(',') else {
        return (None, raw.to_string());
    };

    match pid.trim().parse::<u32>() {
        Ok(pid) => (Some(pid), process.trim().to_string()),
        Err(_) => (None, raw.to_string()),
    }
}

fn public_owner_process(process: &str) -> String {
    let process = process.trim();

    if is_known_public_owner_process(process) {
        process.to_string()
    } else {
        "other process".to_string()
    }
}

fn public_owner_raw(raw: &str, pid: Option<u32>, process: &str) -> String {
    if is_known_public_owner_process(process) {
        raw.to_string()
    } else if let Some(pid) = pid {
        format!("pid {pid}, other process")
    } else {
        "other process".to_string()
    }
}

fn public_owner_node_name(node_name: &str, process: &str) -> String {
    if is_known_public_owner_process(process) {
        node_name.to_string()
    } else {
        "other process".to_string()
    }
}

fn is_known_public_owner_process(process: &str) -> bool {
    let process = process.to_lowercase();

    matches!(
        process.as_str(),
        "midiserver" | "usbaudiod" | "ptpcamerad" | "unknown owner"
    ) || process.contains("field kit")
        || process.contains("fieldkit")
        || process.contains("android file transfer")
        || process.contains("openmtp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ioreg_device_and_interface_owners() {
        let output = r#"
+-o TP-7@01100000  <class IOUSBHostDevice, id 0x1000b6528, registered>
  | {
  |   "idVendor" = 9063
  |   "idProduct" = 25
  |   "UsbExclusiveOwner" = "pid 1490, MIDIServer"
  | }
  |
  +-o Audio In 0@1  <class IOUSBHostInterface, id 0x1000b6530, registered>
  | | {
  | |   "bInterfaceProtocol" = 0
  | |   "bInterfaceClass" = 1
  | |   "bInterfaceSubClass" = 2
  | |   "UsbExclusiveOwner" = "pid 88951, usbaudiod"
  | |   "kUSBString" = "Audio In 0"
  | |   "bInterfaceNumber" = 1
  | | }
  | |
  | +-o usbaudiod@01100000  <class AppleUSBHostFrameworkInterfaceClient, id 0x1000b6545>
  |     {
  |       "IOUserClientCreator" = "pid 88951, usbaudiod"
  |     }
  |
  +-o field kit  <class AppleUSBHostDeviceUserClient, id 0x1000b6537>
      {
        "IOUserClientCreator" = "pid 19731, field kit"
      }
"#;

        let owners = parse_ioreg_usb_owners(output);

        assert_eq!(owners.len(), 4);
        assert_eq!(owners[0].scope, UsbOwnerScope::Device);
        assert_eq!(owners[0].pid, Some(1490));
        assert_eq!(owners[1].scope, UsbOwnerScope::Interface);
        assert_eq!(owners[1].interface_number, Some(1));
        assert_eq!(owners[1].interface_name.as_deref(), Some("Audio In 0"));
        assert_eq!(owners[2].scope, UsbOwnerScope::Interface);
        assert_eq!(owners[2].interface_number, Some(1));
        assert_eq!(owners[3].scope, UsbOwnerScope::Device);
        assert!(is_conflicting_owner(&owners[3]));
    }

    #[test]
    fn parses_owner_without_pid() {
        let (pid, process) = parse_owner("unknown owner");

        assert_eq!(pid, None);
        assert_eq!(process, "unknown owner");
    }

    #[test]
    fn redacts_unrecognized_usb_owner_processes() {
        let output = r#"
+-o TP-7@01100000  <class IOUSBHostDevice, id 0x1000b6528, registered>
  | {
  |   "idVendor" = 9063
  |   "idProduct" = 25
  | }
  |
  +-o companion  <class AppleUSBHostDeviceUserClient, id 0x1000b6537>
      {
        "IOUserClientCreator" = "pid 92001, Companion Helper"
      }
"#;

        let owners = parse_ioreg_usb_owners(output);

        assert_eq!(owners.len(), 1);
        assert_eq!(owners[0].pid, Some(92001));
        assert_eq!(owners[0].process, "other process");
        assert_eq!(owners[0].raw, "pid 92001, other process");
        assert_eq!(owners[0].owner_node_name, "other process");
    }

    #[test]
    fn selects_only_exclusive_ptpcamerad_interface_owners() {
        let output = r#"
+-o TP-7 MTP Device@01100000  <class IOUSBHostDevice, id 0x1000b6528, registered>
  | {
  |   "idVendor" = 9063
  |   "idProduct" = 25
  | }
  |
  +-o MTP Data Intf@0  <class IOUSBHostInterface, id 0x1000b6530, registered>
  | | {
  | |   "bInterfaceProtocol" = 1
  | |   "bInterfaceClass" = 6
  | |   "bInterfaceSubClass" = 1
  | |   "UsbExclusiveOwner" = "pid 28379, ptpcamerad"
  | |   "kUSBString" = "MTP Data Intf"
  | |   "bInterfaceNumber" = 0
  | | }
  | |
  | +-o ptpcamerad  <class AppleUSBHostInterfaceUserClient, id 0x1000b6545>
  |     {
  |       "IOUserClientCreator" = "pid 28379, ptpcamerad"
  |     }
  |
  +-o field kit  <class AppleUSBHostDeviceUserClient, id 0x1000b6537>
      {
        "IOUserClientCreator" = "pid 19731, field kit"
      }
"#;

        let owners = parse_ioreg_usb_owners(output);

        assert_eq!(ptpcamerad_exclusive_owner_pids(&owners), vec![28379]);
    }
}
