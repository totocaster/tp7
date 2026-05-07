use serde::Serialize;
use std::path::Path;
use std::process::Command;

use crate::device::{Tp7Device, UsbMode, filter_by_serial, list_tp7_devices};
use crate::output::AppError;
use crate::usb_owner::{UsbOwner, inspect_tp7_usb_owners, is_conflicting_owner};

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub devices: Vec<Tp7Device>,
    pub checks: Vec<DoctorCheck>,
    pub process_conflicts: Vec<ProcessConflict>,
    pub usb_owners: Vec<UsbOwner>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Error,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            CheckStatus::Ok => "ok",
            CheckStatus::Warn => "warn",
            CheckStatus::Error => "error",
        };
        f.write_str(label)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheck {
    pub status: CheckStatus,
    pub name: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProcessConflict {
    pub pid: u32,
    pub name: String,
    pub reason: String,
}

pub fn run_doctor(serial: Option<&str>) -> Result<DoctorReport, AppError> {
    let devices = list_tp7_devices()?;
    let devices = filter_by_serial(devices, serial)?;
    let process_conflicts = find_process_conflicts().unwrap_or_default();
    let usb_owners = inspect_tp7_usb_owners().unwrap_or_default();
    let mut checks = Vec::new();

    if devices.is_empty() {
        checks.push(DoctorCheck {
            status: CheckStatus::Error,
            name: "tp7-detected".to_string(),
            message: "No TP-7 device was found over USB.".to_string(),
        });
    } else {
        checks.push(DoctorCheck {
            status: CheckStatus::Ok,
            name: "tp7-detected".to_string(),
            message: format!("Found {} TP-7 device(s).", devices.len()),
        });
    }

    match devices.as_slice() {
        [] => {}
        [device] => checks.push(mode_check(device)),
        _ => checks.push(DoctorCheck {
            status: CheckStatus::Warn,
            name: "multiple-devices".to_string(),
            message: "Multiple TP-7 devices are connected. Use --device <serial> once device-specific commands are implemented.".to_string(),
        }),
    }

    if process_conflicts.is_empty() {
        checks.push(DoctorCheck {
            status: CheckStatus::Ok,
            name: "process-conflicts".to_string(),
            message: "No known MTP/TE companion app conflicts were detected.".to_string(),
        });
    } else {
        checks.push(DoctorCheck {
            status: CheckStatus::Warn,
            name: "process-conflicts".to_string(),
            message: format!(
                "Detected {} process(es) that may compete for TP-7 USB/MTP access.",
                process_conflicts.len()
            ),
        });
    }

    let conflicting_usb_owners = usb_owners
        .iter()
        .filter(|owner| is_conflicting_owner(owner))
        .count();

    if usb_owners.is_empty() {
        checks.push(DoctorCheck {
            status: CheckStatus::Ok,
            name: "usb-owners".to_string(),
            message: "No macOS USB owner records were reported for TP-7.".to_string(),
        });
    } else if conflicting_usb_owners == 0 {
        checks.push(DoctorCheck {
            status: CheckStatus::Ok,
            name: "usb-owners".to_string(),
            message: format!(
                "macOS reports {} TP-7 USB owner record(s); none are known MTP companion app conflicts.",
                usb_owners.len()
            ),
        });
    } else {
        checks.push(DoctorCheck {
            status: CheckStatus::Warn,
            name: "usb-owners".to_string(),
            message: format!(
                "macOS reports {} TP-7 USB owner record(s), including {} likely companion app conflict(s).",
                usb_owners.len(),
                conflicting_usb_owners
            ),
        });
    }

    checks.push(DoctorCheck {
        status: CheckStatus::Ok,
        name: "implementation-dependency".to_string(),
        message: "This CLI uses direct USB enumeration; companion apps are not implementation dependencies.".to_string(),
    });
    checks.push(finder_mount_runtime_check());

    Ok(DoctorReport {
        devices,
        checks,
        process_conflicts,
        usb_owners,
    })
}

fn mode_check(device: &Tp7Device) -> DoctorCheck {
    match device.mode {
        UsbMode::Mtp => DoctorCheck {
            status: CheckStatus::Ok,
            name: "visible-mtp-mode".to_string(),
            message: "The TP-7 currently exposes an MTP-compatible interface.".to_string(),
        },
        UsbMode::AudioMidi => DoctorCheck {
            status: CheckStatus::Warn,
            name: "visible-mtp-mode".to_string(),
            message: "The TP-7 is currently in audio/MIDI mode. MTP file commands will need the TP-7 mode-switch path.".to_string(),
        },
        UsbMode::MassStorage => DoctorCheck {
            status: CheckStatus::Warn,
            name: "visible-mtp-mode".to_string(),
            message: "The TP-7 appears as mass storage, not MTP. This is unexpected for current TP-7 research.".to_string(),
        },
        UsbMode::Mixed => DoctorCheck {
            status: CheckStatus::Warn,
            name: "visible-mtp-mode".to_string(),
            message: "The TP-7 exposes a mixed set of USB interfaces. Verify before claiming interfaces.".to_string(),
        },
        UsbMode::Unknown => DoctorCheck {
            status: CheckStatus::Warn,
            name: "visible-mtp-mode".to_string(),
            message: "The TP-7 USB mode could not be inferred from visible interfaces.".to_string(),
        },
    }
}

fn finder_mount_runtime_check() -> DoctorCheck {
    if fuse_runtime_installed() {
        DoctorCheck {
            status: CheckStatus::Ok,
            name: "finder-mount-runtime".to_string(),
            message: "A FUSE runtime appears installed for Finder mounting. If mounting is still blocked, approve macFUSE in System Settings -> Privacy & Security.".to_string(),
        }
    } else {
        DoctorCheck {
            status: CheckStatus::Warn,
            name: "finder-mount-runtime".to_string(),
            message: "Finder mounting requires macFUSE. Install with `brew install --cask macfuse`; if macOS prompts, approve it in System Settings -> Privacy & Security.".to_string(),
        }
    }
}

fn fuse_runtime_installed() -> bool {
    [
        "/Library/Filesystems/macfuse.fs",
        "/Library/Filesystems/fuse-t.fs",
    ]
    .iter()
    .any(|path| Path::new(path).exists())
}

fn find_process_conflicts() -> Result<Vec<ProcessConflict>, AppError> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,comm=,args="])
        .output()
        .map_err(|error| AppError::ProcessInspection {
            message: error.to_string(),
        })?;

    if !output.status.success() {
        return Err(AppError::ProcessInspection {
            message: format!("ps exited with {}", output.status),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(find_process_conflicts_in_ps(&stdout))
}

fn find_process_conflicts_in_ps(ps_output: &str) -> Vec<ProcessConflict> {
    ps_output
        .lines()
        .filter_map(parse_ps_line)
        .filter_map(|(pid, command, args)| {
            conflict_reason(&command, &args).map(|reason| ProcessConflict {
                pid,
                name: command,
                reason,
            })
        })
        .collect()
}

fn parse_ps_line(line: &str) -> Option<(u32, String, String)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let mut parts = line.splitn(2, char::is_whitespace);
    let pid = parts.next()?.parse().ok()?;
    let rest = parts.next()?.trim();
    let mut rest_parts = rest.splitn(2, char::is_whitespace);
    let command = rest_parts.next()?.to_string();
    let args = rest_parts.next().unwrap_or("").to_string();

    Some((pid, command, args))
}

fn conflict_reason(command: &str, args: &str) -> Option<String> {
    let haystack = format!("{command} {args}").to_lowercase();

    if haystack.contains("fieldkit.app") || haystack.contains("/fieldkit") {
        return Some("FieldKit may own the TP-7 USB device while it is open.".to_string());
    }

    if haystack.contains("android file transfer") || haystack.contains("android-file-transfer") {
        return Some("Android File Transfer-style tools commonly claim MTP devices.".to_string());
    }

    if haystack.contains("openmtp") {
        return Some("OpenMTP commonly claims MTP devices.".to_string());
    }

    if haystack.contains("ptpcamerad") {
        return Some("macOS ptpcamerad may claim PTP/MTP-class devices.".to_string());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_process_conflicts_from_ps_output() {
        let output = "\
19731 /Applications/Fi /Applications/FieldKit.app/Contents/MacOS/field kit
42 /usr/bin/true true
";

        let conflicts = find_process_conflicts_in_ps(output);

        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].pid, 19731);
        assert!(conflicts[0].reason.contains("FieldKit"));
    }

    #[test]
    fn ignores_empty_and_unrelated_processes() {
        let conflicts = find_process_conflicts_in_ps("\n123 /usr/bin/zsh zsh\n");

        assert!(conflicts.is_empty());
    }
}
