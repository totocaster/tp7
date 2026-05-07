use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use fuser::{Config, MountOption};
use mtp_mount::fs::MtpFs;
use serde::Serialize;

use crate::device::{Tp7Device, UsbMode};
use crate::mtp_session::{MtpOpenPolicy, open_mtp_session};
use crate::output::AppError;

const DEFAULT_MOUNTPOINT_NAME: &str = "TP-7";
const MAX_DEFAULT_MOUNTPOINT_ATTEMPTS: usize = 99;

#[derive(Debug, Clone, Serialize)]
pub struct MountReport {
    pub mountpoint: String,
    pub serial_number: Option<String>,
    pub initial_mode: UsbMode,
    pub final_mode: UsbMode,
    pub read_only: bool,
    pub opened_finder: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmountReport {
    pub mountpoint: String,
    pub force: bool,
    pub unmounted: bool,
    pub message: String,
}

pub fn run_mount(
    serial: Option<&str>,
    auto_connect: bool,
    mountpoint: Option<&str>,
    read_only: bool,
    open_finder: bool,
    human_status: bool,
) -> Result<MountReport, AppError> {
    let mountpoint = prepare_mountpoint(mountpoint)?;
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| AppError::Runtime {
            message: error.to_string(),
        })?;

    let session = runtime.block_on(open_mtp_session(serial, policy))?;
    let prepared = session.prepared;
    let device = session.device;
    let mountpoint_label = path_to_string(&mountpoint);
    let serial_number = prepared.usb.serial_number.clone();
    let initial_mode = prepared.initial_usb.mode.clone();
    let final_mode = prepared.usb.mode.clone();

    let mtp_fs = MtpFs::new(device, read_only, runtime.handle().clone());
    let mut config = Config::default();
    config.mount_options = mount_options(&mtp_fs, &prepared.usb);

    let background = fuser::spawn_mount2(mtp_fs, &mountpoint, &config).map_err(|error| {
        AppError::Mount {
            message: format!(
                "failed to mount {mountpoint_label}: {error}. Install macFUSE with `brew install --cask macfuse`; if macOS prompts, approve it in System Settings -> Privacy & Security."
            ),
        }
    })?;

    let opened_finder = if open_finder {
        open_mountpoint_in_finder(&mountpoint, human_status)
    } else {
        false
    };

    if human_status {
        let access_mode = if read_only { "read-only" } else { "read-write" };
        println!("mounted TP-7 at {mountpoint_label} ({access_mode})");
        println!("unmount from Finder or run: tp7 unmount {mountpoint_label}");
        let _ = io::stdout().flush();
    }

    let join_result = background.join();
    let message = match join_result {
        Ok(()) => "Unmounted by the OS.".to_string(),
        Err(error) if is_graceful_mount_end(&error) => "Unmounted by the OS.".to_string(),
        Err(error) => {
            return Err(AppError::Mount {
                message: format!("mounted filesystem ended unexpectedly: {error}"),
            });
        }
    };

    Ok(MountReport {
        mountpoint: mountpoint_label,
        serial_number,
        initial_mode,
        final_mode,
        read_only,
        opened_finder,
        message,
    })
}

pub fn run_unmount(mountpoint: Option<&str>, force: bool) -> Result<UnmountReport, AppError> {
    let mountpoint = resolve_unmount_mountpoint(mountpoint)?;
    let mountpoint_label = path_to_string(&mountpoint);

    if !is_mounted_at(&mountpoint).map_err(|message| AppError::Unmount { message })? {
        return Ok(UnmountReport {
            mountpoint: mountpoint_label,
            force,
            unmounted: false,
            message: "Mount point is not currently mounted.".to_string(),
        });
    }

    match run_diskutil_unmount(&mountpoint, force) {
        Ok(()) => Ok(UnmountReport {
            mountpoint: mountpoint_label,
            force,
            unmounted: true,
            message: "Unmounted.".to_string(),
        }),
        Err(diskutil_error) => {
            if !is_mounted_at(&mountpoint).map_err(|message| AppError::Unmount { message })? {
                return Ok(UnmountReport {
                    mountpoint: mountpoint_label,
                    force,
                    unmounted: true,
                    message: "Unmounted.".to_string(),
                });
            }

            match run_umount(&mountpoint, force) {
                Ok(()) => Ok(UnmountReport {
                    mountpoint: mountpoint_label,
                    force,
                    unmounted: true,
                    message: "Unmounted.".to_string(),
                }),
                Err(umount_error) => Err(AppError::Unmount {
                    message: format!(
                        "failed to unmount {mountpoint_label} with diskutil ({diskutil_error}) or umount ({umount_error})"
                    ),
                }),
            }
        }
    }
}

fn mount_options(fs: &MtpFs, device: &Tp7Device) -> Vec<MountOption> {
    let mut options = fs.mount_options();
    options.retain(|option| !matches!(option, MountOption::FSName(_) | MountOption::Subtype(_)));

    let fs_name = match device.serial_number.as_deref() {
        Some(serial) => format!("tp7:{serial}"),
        None => "tp7".to_string(),
    };

    options.push(MountOption::FSName(fs_name));
    options.push(MountOption::Subtype("mtp".to_string()));
    options.push(MountOption::CUSTOM("volname=TP-7".to_string()));
    options
}

fn prepare_mountpoint(path: Option<&str>) -> Result<PathBuf, AppError> {
    match path {
        Some(path) => prepare_explicit_mountpoint(path),
        None => prepare_default_mountpoint(),
    }
}

fn prepare_explicit_mountpoint(path: &str) -> Result<PathBuf, AppError> {
    let path = absolute_path(path)?;
    if !path.exists() {
        return create_mountpoint_dir(&path, false);
    }

    let path = canonicalize_path(&path)?;
    validate_available_mountpoint(&path, false)?;
    Ok(path)
}

fn prepare_default_mountpoint() -> Result<PathBuf, AppError> {
    for index in 1..=MAX_DEFAULT_MOUNTPOINT_ATTEMPTS {
        let candidate = default_mountpoint_candidate(index);
        if !candidate.exists() {
            return create_mountpoint_dir(&candidate, true);
        }

        let Ok(candidate) = canonicalize_path(&candidate) else {
            continue;
        };
        if validate_available_mountpoint(&candidate, true).is_ok() {
            return Ok(candidate);
        }
    }

    Err(AppError::Mount {
        message: format!(
            "no available default mount point found under {}; pass an explicit mount point",
            default_mountpoint_base_label()
        ),
    })
}

fn default_mountpoint_candidate(index: usize) -> PathBuf {
    let base = default_mountpoint_base();
    if index == 1 {
        base
    } else {
        numbered_default_mountpoint_base(base, index)
    }
}

fn default_mountpoint_base() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home).join(DEFAULT_MOUNTPOINT_NAME),
        _ => PathBuf::from(DEFAULT_MOUNTPOINT_NAME),
    }
}

fn numbered_default_mountpoint_base(base: PathBuf, index: usize) -> PathBuf {
    let mut name = base
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| DEFAULT_MOUNTPOINT_NAME.into());
    name.push(format!("-{index}"));

    match base.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

fn default_mountpoint_base_label() -> String {
    match std::env::var_os("HOME") {
        Some(home) if !home.is_empty() => {
            format!("{}/{}", Path::new(&home).display(), DEFAULT_MOUNTPOINT_NAME)
        }
        _ => DEFAULT_MOUNTPOINT_NAME.to_string(),
    }
}

fn create_mountpoint_dir(path: &Path, default_mountpoint: bool) -> Result<PathBuf, AppError> {
    fs::create_dir_all(path).map_err(|error| AppError::FileSystem {
        path: path_to_string(path),
        message: if default_mountpoint {
            format!("could not create default mount point: {error}. Pass an explicit empty directory you own")
        } else {
            error.to_string()
        },
    })?;

    let path = canonicalize_path(path)?;
    validate_available_mountpoint(&path, default_mountpoint)?;
    Ok(path)
}

fn validate_available_mountpoint(path: &Path, default_mountpoint: bool) -> Result<(), AppError> {
    if !path.is_dir() {
        return Err(AppError::FileSystem {
            path: path_to_string(path),
            message: "mount point must be a directory".to_string(),
        });
    }

    if is_mounted_at(path).map_err(|message| AppError::Mount { message })? {
        return Err(AppError::Mount {
            message: format!("mount point is already mounted: {}", path.display()),
        });
    }

    if !dir_is_empty(path)? {
        let message = if default_mountpoint {
            "default mount point is not empty; trying the next default candidate".to_string()
        } else {
            "mount point must be empty".to_string()
        };
        return Err(AppError::FileSystem {
            path: path_to_string(path),
            message,
        });
    }

    Ok(())
}

fn dir_is_empty(path: &Path) -> Result<bool, AppError> {
    let mut entries = fs::read_dir(path).map_err(|error| AppError::FileSystem {
        path: path_to_string(path),
        message: error.to_string(),
    })?;

    Ok(entries.next().is_none())
}

fn resolve_unmount_mountpoint(path: Option<&str>) -> Result<PathBuf, AppError> {
    match path {
        Some(path) => resolve_explicit_unmount_mountpoint(path),
        None => resolve_default_unmount_mountpoint(),
    }
}

fn resolve_explicit_unmount_mountpoint(path: &str) -> Result<PathBuf, AppError> {
    let path = absolute_path(path)?;
    if !path.exists() {
        return Ok(path);
    }

    canonicalize_path(&path)
}

fn resolve_default_unmount_mountpoint() -> Result<PathBuf, AppError> {
    let mountpoints = find_tp7_mountpoints().map_err(|message| AppError::Unmount { message })?;

    match mountpoints.as_slice() {
        [] => Ok(default_mountpoint_base()),
        [mountpoint] => Ok(mountpoint.clone()),
        _ => Err(AppError::Unmount {
            message: format!(
                "multiple TP-7 mounts found: {}; pass the mount point explicitly",
                mountpoints
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }),
    }
}

fn canonicalize_path(path: &Path) -> Result<PathBuf, AppError> {
    path.canonicalize().map_err(|error| AppError::FileSystem {
        path: path_to_string(path),
        message: error.to_string(),
    })
}

fn absolute_path(path: &str) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return Ok(path);
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| AppError::FileSystem {
            path: ".".to_string(),
            message: error.to_string(),
        })
}

fn open_mountpoint_in_finder(path: &Path, human_status: bool) -> bool {
    #[cfg(target_os = "macos")]
    {
        match Command::new("open").arg(path).status() {
            Ok(status) if status.success() => true,
            Ok(status) => {
                if human_status {
                    eprintln!(
                        "warning: could not open Finder for {}: {status}",
                        path.display()
                    );
                }
                false
            }
            Err(error) => {
                if human_status {
                    eprintln!(
                        "warning: could not open Finder for {}: {error}",
                        path.display()
                    );
                }
                false
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = (path, human_status);
        false
    }
}

fn run_diskutil_unmount(path: &Path, force: bool) -> Result<(), String> {
    let mut command = Command::new("diskutil");
    command.arg("unmount");
    if force {
        command.arg("force");
    }
    command.arg(path);
    run_unmount_command(command)
}

fn run_umount(path: &Path, force: bool) -> Result<(), String> {
    let mut command = Command::new("umount");
    if force {
        command.arg("-f");
    }
    command.arg(path);
    run_unmount_command(command)
}

fn run_unmount_command(mut command: Command) -> Result<(), String> {
    let output = command.output().map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exited with {}", output.status)
    };

    Err(detail)
}

fn is_mounted_at(path: &Path) -> Result<bool, String> {
    let output = read_mount_output()?;
    Ok(mount_output_has_mountpoint(&output, path))
}

fn find_tp7_mountpoints() -> Result<Vec<PathBuf>, String> {
    let output = read_mount_output()?;
    Ok(mount_output_tp7_mountpoints(&output))
}

fn read_mount_output() -> Result<String, String> {
    let output = Command::new("mount")
        .output()
        .map_err(|error| format!("failed to inspect mounted filesystems: {error}"))?;

    if !output.status.success() {
        return Err(format!("mount exited with {}", output.status));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn mount_output_has_mountpoint(output: &str, path: &Path) -> bool {
    let needle = format!(" on {} (", path.display());
    output.lines().any(|line| line.contains(&needle))
}

fn mount_output_tp7_mountpoints(output: &str) -> Vec<PathBuf> {
    output
        .lines()
        .filter_map(parse_tp7_mountpoint)
        .collect::<Vec<_>>()
}

fn parse_tp7_mountpoint(line: &str) -> Option<PathBuf> {
    let on_index = line.find(" on ")?;
    let options_index = line.rfind(" (")?;
    if options_index <= on_index {
        return None;
    }

    let source = &line[..on_index];
    let mountpoint = &line[on_index + 4..options_index];
    let options = &line[options_index + 2..];

    if source.starts_with("tp7") && options.contains("mtp") {
        Some(PathBuf::from(mountpoint))
    } else {
        None
    }
}

fn is_graceful_mount_end(error: &io::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("not mounted")
        || message.contains("unmounted")
        || message.contains("transport endpoint is not connected")
        || message.contains("device not configured")
        || message.contains("no such file or directory")
        || message.contains("invalid argument")
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mountpoint_in_mount_output() {
        let output = "/dev/disk3s1 on /System/Volumes/Data (apfs, local)\ntp7:F1RTL11C on /Users/toto/TP-7 (mtp, nodev, nosuid, read-only)\n";

        assert!(mount_output_has_mountpoint(
            output,
            Path::new("/Users/toto/TP-7")
        ));
    }

    #[test]
    fn ignores_prefix_mountpoint_matches() {
        let output = "tp7:F1RTL11C on /Users/toto/TP-7-backup (mtp, nodev, nosuid, read-only)\n";

        assert!(!mount_output_has_mountpoint(
            output,
            Path::new("/Users/toto/TP-7")
        ));
    }

    #[test]
    fn treats_common_unmount_errors_as_graceful() {
        let error = io::Error::other("mount point is not mounted");

        assert!(is_graceful_mount_end(&error));
    }

    #[test]
    fn finds_tp7_mountpoints_in_mount_output() {
        let output = "/dev/disk3s1 on /System/Volumes/Data (apfs, local)\ntp7:F1RTL11C on /Volumes/TP-7 (mtp, nodev, nosuid, read-only)\ntp7:F2RTL11C on /Volumes/TP-7-2 (mtp, nodev, nosuid, read-only)\n";

        assert_eq!(
            mount_output_tp7_mountpoints(output),
            vec![
                PathBuf::from("/Volumes/TP-7"),
                PathBuf::from("/Volumes/TP-7-2")
            ]
        );
    }
}
