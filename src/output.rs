use serde::Serialize;
use thiserror::Error;

use crate::connect::ConnectReport;
use crate::device::{Tp7Device, interface_summary};
use crate::doctor::{DoctorReport, ProcessConflict};
use crate::eject::EjectReport;
use crate::ls::{LsEntry, LsReport};
use crate::pull::{PullReport, PullStatus};
use crate::push::{PushReport, PushStatus};
use crate::remote::ObjectKind;
use crate::stat::StatReport;
use crate::status::StatusReport;
use crate::tree::TreeReport;
use crate::usb_owner::UsbOwner;
use crate::write_ops::{MkdirReport, RenameReport, RmReport};

#[derive(Debug, Clone, Copy)]
pub struct LsDisplayOptions {
    pub long: bool,
    pub ids: bool,
    pub size: bool,
    pub human_readable: bool,
    pub sort: LsSort,
    pub reverse: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LsSort {
    Name,
    Size,
    Time,
}

#[derive(Debug, Error)]
pub enum AppError {
    #[error("USB enumeration failed: {message}")]
    UsbEnumeration { message: String },

    #[error("process inspection failed: {message}")]
    ProcessInspection { message: String },

    #[error("USB ownership inspection failed: {message}")]
    UsbOwnershipInspection { message: String },

    #[error("no TP-7 device with serial {serial} was found")]
    DeviceNotFound { serial: String },

    #[error("no TP-7 devices were found")]
    NoDevices,

    #[error("found {count} TP-7 devices; rerun with --device <serial>")]
    MultipleDevices { count: usize },

    #[error("TP-7 {serial} is in {mode} mode; no MTP-compatible interface is visible")]
    MtpNotVisible { serial: String, mode: String },

    #[error(
        "TP-7 {serial} is in {mode} mode; rerun with --auto-connect to switch into MTP mode for this command"
    )]
    AutoConnectRequired { serial: String, mode: String },

    #[error("invalid remote path {path}: {message}")]
    InvalidRemotePath { path: String, message: String },

    #[error("remote path was not found: {path}")]
    RemotePathNotFound { path: String },

    #[error("remote path is not a folder: {path}")]
    RemotePathNotDirectory { path: String },

    #[error("remote path is the storage root; use `tp7 ls /` to inspect it")]
    RemotePathIsRoot,

    #[error("remote path is a folder; rerun with --recursive: {path}")]
    RemotePathIsFolder { path: String },

    #[error("local path already exists; use --overwrite or --skip-existing: {path}")]
    LocalPathExists { path: String },

    #[error("local path is a directory: {path}")]
    LocalPathIsDirectory { path: String },

    #[error("local path is a file: {path}")]
    LocalPathIsFile { path: String },

    #[error("local path is a folder; use --recursive to upload it: {path}")]
    LocalPathIsFolder { path: String },

    #[error("remote path already exists; use --overwrite to replace it: {path}")]
    RemotePathExists { path: String },

    #[error("file operation failed for {path}: {message}")]
    FileSystem { path: String, message: String },

    #[error(
        "download verification failed for {path}: expected {expected_size} bytes, got {actual_size}"
    )]
    TransferVerification {
        path: String,
        expected_size: u64,
        actual_size: u64,
    },

    #[error("invalid arguments: {message}")]
    InvalidArguments { message: String },

    #[error("MTP operation failed: {message}")]
    Mtp { message: String },

    #[error("could not take over the TP-7 MTP interface: {message}")]
    MtpTakeover { message: String },

    #[error("MTP operation is not supported: {message}")]
    MtpUnsupported { message: String },

    #[error("MTP device is busy or owned by another process: {message}")]
    MtpExclusiveAccess { message: String },

    #[error("MIDI operation failed: {message}")]
    Midi { message: String },

    #[error("MIDI response timed out: {message}")]
    MidiTimeout { message: String },

    #[error("MIDI command 0x{command:02x} was rejected with status 0x{status:02x}: {message}")]
    MidiCommandRejected {
        command: u8,
        status: u8,
        message: String,
    },

    #[error("runtime initialization failed: {message}")]
    Runtime { message: String },

    #[error("JSON output failed: {source}")]
    Json { source: serde_json::Error },

    #[error("{command} is not implemented yet")]
    NotImplemented { command: String },
}

impl AppError {
    pub fn not_implemented(command: impl Into<String>) -> Self {
        Self::NotImplemented {
            command: command.into(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            AppError::NotImplemented { .. }
            | AppError::InvalidRemotePath { .. }
            | AppError::RemotePathIsRoot
            | AppError::RemotePathIsFolder { .. }
            | AppError::LocalPathExists { .. }
            | AppError::LocalPathIsDirectory { .. }
            | AppError::LocalPathIsFile { .. }
            | AppError::LocalPathIsFolder { .. }
            | AppError::RemotePathExists { .. }
            | AppError::InvalidArguments { .. } => 2,
            AppError::MtpNotVisible { .. } | AppError::AutoConnectRequired { .. } => 3,
            AppError::MtpExclusiveAccess { .. } | AppError::MtpTakeover { .. } => 4,
            AppError::Midi { .. }
            | AppError::MidiTimeout { .. }
            | AppError::MidiCommandRejected { .. } => 5,
            AppError::UsbEnumeration { .. }
            | AppError::ProcessInspection { .. }
            | AppError::UsbOwnershipInspection { .. }
            | AppError::DeviceNotFound { .. }
            | AppError::NoDevices
            | AppError::MultipleDevices { .. }
            | AppError::RemotePathNotFound { .. }
            | AppError::RemotePathNotDirectory { .. }
            | AppError::FileSystem { .. }
            | AppError::TransferVerification { .. }
            | AppError::Mtp { .. }
            | AppError::MtpUnsupported { .. }
            | AppError::Runtime { .. }
            | AppError::Json { .. } => 1,
        }
    }
}

pub fn write_devices(devices: &[Tp7Device], json: bool) -> Result<(), AppError> {
    if json {
        write_json(devices)?;
        return Ok(());
    }

    if devices.is_empty() {
        println!("No TP-7 devices found.");
        return Ok(());
    }

    for device in devices {
        println!(
            "{}  {}  {}:{}  {}  {}",
            device.serial_number.as_deref().unwrap_or("<no-serial>"),
            device.mode,
            device.vendor_id_hex,
            device.product_id_hex,
            device.speed.as_deref().unwrap_or("unknown-speed"),
            device.product.as_deref().unwrap_or("TP-7")
        );
        println!("  manufacturer: {}", display_opt(&device.manufacturer));
        println!("  usb version: {}", device.usb_version);
        println!(
            "  device version: {}",
            device.device_version.as_deref().unwrap_or("unknown")
        );
        if let Some(location_id) = &device.location_id {
            println!("  macOS location id: {location_id}");
        }
        if let Some(bus_id) = &device.bus_id {
            println!("  bus: {bus_id}");
        }
        if let Some(address) = device.device_address {
            println!("  address: {address}");
        }
        println!("  interfaces: {}", interface_summary(&device.interfaces));
    }

    Ok(())
}

pub fn write_status(report: &StatusReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!(
        "{}  {}  {}",
        report.usb.serial_number.as_deref().unwrap_or("<no-serial>"),
        report.usb.mode,
        report.usb.product.as_deref().unwrap_or("TP-7")
    );
    println!(
        "  mtp: {} {} ({})",
        report.mtp.manufacturer, report.mtp.model, report.mtp.device_version
    );
    println!("  mtp serial: {}", report.mtp.serial_number);
    println!("  supports rename: {}", report.mtp.supports_rename);
    println!("  storages: {}", report.mtp.storage_count);

    for storage in &report.mtp.storages {
        println!(
            "  - {}: {} free of {} bytes ({})",
            storage.id, storage.free_space_bytes, storage.max_capacity_bytes, storage.description
        );
    }

    Ok(())
}

pub fn write_connect(report: &ConnectReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!(
        "{}  {} -> {}  {}",
        report.serial_number.as_deref().unwrap_or("<no-serial>"),
        report.initial_mode,
        report.final_mode,
        report.message
    );
    println!("  switched: {}", if report.switched { "yes" } else { "no" });
    println!("  mtp session: {}", report.mtp_session.message);
    if let Some(storage_count) = report.mtp_session.storage_count {
        println!("  storages: {storage_count}");
    }
    if let Some(midi_switch) = &report.midi_switch {
        println!(
            "  midi switch: command 0x{:02x}, payload {}",
            midi_switch.command,
            midi_switch.payload.join(" ")
        );
    }

    Ok(())
}

pub fn write_ls(report: &LsReport, json: bool, options: LsDisplayOptions) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    let mut entries = report.entries.clone();
    sort_ls_entries(&mut entries, options.sort);
    if options.reverse {
        entries.reverse();
    }

    for entry in &entries {
        let name = ls_display_name(entry.name.as_str(), &entry.kind);
        let size = ls_size_label(entry.size, options.human_readable);

        match (options.long, options.ids, options.size) {
            (true, true, _) => println!(
                "{:>10} {:<6} {:>12} {:<15} {}",
                entry.id,
                ls_kind_label(&entry.kind),
                size,
                entry.modified.as_deref().unwrap_or("-"),
                name
            ),
            (true, false, _) => println!(
                "{:<6} {:>12} {:<15} {}",
                ls_kind_label(&entry.kind),
                size,
                entry.modified.as_deref().unwrap_or("-"),
                name
            ),
            (false, true, true) => println!("{:>10} {size:>12} {name}", entry.id),
            (false, true, false) => println!("{:>10} {name}", entry.id),
            (false, false, true) => println!("{size:>12} {name}"),
            (false, false, false) => println!("{name}"),
        }
    }

    Ok(())
}

pub fn write_stat(report: &StatReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!(
        "{}  {}  {}",
        report.path,
        ls_kind_label(&report.object.kind),
        report.object.name
    );
    println!("  id: {}", report.object.id);
    println!("  parent: {}", report.object.parent_id);
    println!("  storage: {}", report.object.storage_id);
    println!("  size: {}", size_with_bytes(report.object.size));
    println!("  format: {} ({})", report.format, report.format_code);
    println!("  created: {}", report.created.as_deref().unwrap_or("-"));
    println!(
        "  modified: {}",
        report.object.modified.as_deref().unwrap_or("-")
    );

    Ok(())
}

pub fn write_tree(report: &TreeReport, json: bool, ids: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    let entry_is_requested_file =
        report.entries.len() == 1 && report.entries[0].path == report.path;
    let root_indent = if entry_is_requested_file {
        0
    } else {
        println!("{}", report.path);
        1
    };

    for entry in &report.entries {
        let indent = "  ".repeat(entry.depth + root_indent);
        let name = ls_display_name(entry.object.name.as_str(), &entry.object.kind);

        if ids {
            println!("{indent}{:>10} {name}", entry.object.id);
        } else {
            println!("{indent}{name}");
        }
    }

    Ok(())
}

pub fn write_pull(report: &PullReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    for file in &report.files {
        let status = pull_status_label(&file.status);
        println!(
            "{status} {} -> {} ({})",
            file.remote_path,
            file.local_path,
            size_with_bytes(file.size)
        );
    }

    if report.dry_run {
        let planned_bytes = report.files.iter().map(|file| file.size).sum();
        println!(
            "{} downloaded, {} skipped, {} would download",
            report.downloaded,
            report.skipped,
            size_with_bytes(planned_bytes)
        );
    } else {
        println!(
            "{} downloaded, {} skipped, {}",
            report.downloaded,
            report.skipped,
            size_with_bytes(report.total_bytes)
        );
    }

    Ok(())
}

pub fn write_push(report: &PushReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    for file in &report.files {
        let status = push_status_label(&file.status);
        println!(
            "{status} {} -> {} ({})",
            file.local_path,
            file.remote_path,
            size_with_bytes(file.size)
        );
    }

    if report.dry_run {
        let planned_bytes = report.files.iter().map(|file| file.size).sum();
        println!(
            "{} uploaded, {} would upload",
            report.uploaded,
            size_with_bytes(planned_bytes)
        );
    } else {
        println!(
            "{} uploaded, {}",
            report.uploaded,
            size_with_bytes(report.total_bytes)
        );
    }

    Ok(())
}

pub fn write_mkdir(report: &MkdirReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    if report.created.is_empty() {
        println!("exists {}", report.path);
    } else {
        for path in &report.created {
            println!("created {path}");
        }
    }

    Ok(())
}

pub fn write_rename(report: &RenameReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!("renamed {} -> {}", report.old_path, report.new_path);

    Ok(())
}

pub fn write_rm(report: &RmReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    if report.removed.is_empty() {
        println!("nothing removed");
        return Ok(());
    }

    for object in &report.removed {
        let status = if report.dry_run {
            "would remove"
        } else {
            "removed"
        };
        println!("{status} {} ({})", report.path, ls_kind_label(&object.kind));
    }

    Ok(())
}

pub fn write_eject(report: &EjectReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!(
        "{}  {} -> {}  {}",
        report.serial_number.as_deref().unwrap_or("<no-serial>"),
        report.initial_mode,
        report.final_mode,
        report.message
    );
    println!("  switched: {}", if report.switched { "yes" } else { "no" });
    println!("  closed: {}", if report.closed { "yes" } else { "no" });

    Ok(())
}

pub fn write_doctor(report: &DoctorReport, json: bool) -> Result<(), AppError> {
    if json {
        write_json(report)?;
        return Ok(());
    }

    println!("TP-7 doctor");
    for check in &report.checks {
        println!("[{}] {}: {}", check.status, check.name, check.message);
    }

    if !report.process_conflicts.is_empty() {
        println!();
        println!("Possible process conflicts:");
        for conflict in &report.process_conflicts {
            print_process_conflict(conflict);
        }
    }

    if !report.usb_owners.is_empty() {
        println!();
        println!("macOS USB owners:");
        for owner in &report.usb_owners {
            print_usb_owner(owner);
        }
    }

    if !report.devices.is_empty() {
        println!();
        println!("Detected devices:");
        for device in &report.devices {
            println!(
                "- {} ({})",
                device.serial_number.as_deref().unwrap_or("<no-serial>"),
                device.mode
            );
        }
    }

    Ok(())
}

fn write_json<T: Serialize + ?Sized>(value: &T) -> Result<(), AppError> {
    serde_json::to_writer_pretty(std::io::stdout(), value)
        .map_err(|source| AppError::Json { source })?;
    println!();
    Ok(())
}

fn display_opt(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("unknown")
}

fn ls_kind_label(kind: &ObjectKind) -> &'static str {
    match kind {
        ObjectKind::File => "file",
        ObjectKind::Folder => "folder",
    }
}

fn ls_display_name(name: &str, kind: &ObjectKind) -> String {
    match kind {
        ObjectKind::File => name.to_string(),
        ObjectKind::Folder => format!("{name}/"),
    }
}

fn sort_ls_entries(entries: &mut [LsEntry], sort: LsSort) {
    match sort {
        LsSort::Name => {}
        LsSort::Size => entries.sort_by(|left, right| {
            right
                .size
                .cmp(&left.size)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        }),
        LsSort::Time => entries.sort_by(|left, right| {
            right
                .modified
                .cmp(&left.modified)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
                .then_with(|| left.name.cmp(&right.name))
        }),
    }
}

fn ls_size_label(size: u64, human_readable: bool) -> String {
    if !human_readable {
        return size.to_string();
    }

    human_size(size)
}

fn size_with_bytes(size: u64) -> String {
    if size < 1024 {
        format!("{size} bytes")
    } else {
        format!("{} ({size} bytes)", human_size(size))
    }
}

fn human_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = size as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{size}{}", UNITS[unit])
    } else if value >= 10.0 {
        format!("{value:.0}{}", UNITS[unit])
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn pull_status_label(status: &PullStatus) -> &'static str {
    match status {
        PullStatus::Downloaded => "downloaded",
        PullStatus::DryRun => "would download",
        PullStatus::Skipped => "skipped",
    }
}

fn push_status_label(status: &PushStatus) -> &'static str {
    match status {
        PushStatus::Uploaded => "uploaded",
        PushStatus::DryRun => "would upload",
    }
}

fn print_process_conflict(conflict: &ProcessConflict) {
    println!(
        "- pid {}: {} ({})",
        conflict.pid, conflict.name, conflict.reason
    );
}

fn print_usb_owner(owner: &UsbOwner) {
    let pid = owner
        .pid
        .map(|pid| pid.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let location = match owner.interface_number {
        Some(number) => format!(
            "{} {} ({})",
            owner.scope,
            number,
            owner
                .interface_name
                .as_deref()
                .unwrap_or(owner.scope_node_name.as_str())
        ),
        None => format!("{} {}", owner.scope, owner.scope_node_name),
    };

    println!(
        "- {} pid {}: {} on {}",
        owner.kind, pid, owner.process, location
    );
}
