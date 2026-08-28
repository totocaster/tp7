use serde::Serialize;

use mtp_rs::MtpDevice;

use crate::device::Tp7Device;
use crate::mtp_session::{MtpOpenPolicy, block_on, map_mtp_error, open_mtp_session};
use crate::output::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub usb: Tp7Device,
    pub mtp: MtpStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct MtpStatus {
    pub manufacturer: String,
    pub model: String,
    pub device_version: String,
    pub serial_number: String,
    pub vendor_extension: Option<String>,
    pub supports_rename: bool,
    pub storage_count: usize,
    pub storages: Vec<StorageStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageStatus {
    pub id: u64,
    pub description: String,
    pub volume_identifier: String,
    pub max_capacity_bytes: u64,
    pub free_space_bytes: u64,
    pub free_space_objects: Option<u64>,
}

pub fn run_status(serial: Option<&str>) -> Result<StatusReport, AppError> {
    block_on(async {
        let session = open_mtp_session(serial, MtpOpenPolicy::MtpOnly).await?;
        let usb = session.prepared.usb.clone();
        let mtp = read_mtp_status(&session.device).await?;
        session.release().await?;

        Ok(StatusReport { usb, mtp })
    })
}

pub async fn read_mtp_status(device: &MtpDevice) -> Result<MtpStatus, AppError> {
    let device_info = device.device_info().clone();
    let storages = device.storages().await.map_err(map_mtp_error)?;
    let storage_count = storages.len();
    let storages = storages
        .into_iter()
        .map(|storage| {
            let info = storage.info();
            StorageStatus {
                id: storage.id().0,
                description: info.description.clone(),
                volume_identifier: info.volume_identifier.clone(),
                max_capacity_bytes: info.total_capacity,
                free_space_bytes: info.free_space,
                free_space_objects: None,
            }
        })
        .collect();

    let status = MtpStatus {
        manufacturer: device_info.manufacturer,
        model: device_info.model,
        device_version: device_info.device_version,
        serial_number: device_info.serial_number,
        vendor_extension: None,
        supports_rename: device.supports_rename(),
        storage_count,
        storages,
    };

    Ok(status)
}
