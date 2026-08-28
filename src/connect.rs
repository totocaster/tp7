use serde::Serialize;

use crate::device::{Tp7Device, UsbMode};
use crate::midi::MidiSwitchReport;
use crate::mtp_session::{
    MtpOpenPolicy, PreparedMtpDevice, block_on, open_mtp_device, prepare_mtp_device,
    release_mtp_device,
};
use crate::output::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectReport {
    pub serial_number: Option<String>,
    pub initial_mode: UsbMode,
    pub final_mode: UsbMode,
    pub mtp_ready: bool,
    pub switched: bool,
    pub midi_switch: Option<MidiSwitchReport>,
    pub mtp_session: MtpSessionCheck,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MtpSessionCheck {
    pub status: MtpSessionStatus,
    pub storage_count: Option<usize>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MtpSessionStatus {
    Open,
    Busy,
    Failed,
}

pub fn run_connect(serial: Option<&str>) -> Result<ConnectReport, AppError> {
    let prepared = prepare_mtp_device(serial, MtpOpenPolicy::AutoSwitch)?;

    Ok(ready_report(&prepared))
}

fn ready_report(prepared: &PreparedMtpDevice) -> ConnectReport {
    let mtp_session = validate_mtp_session(prepared.usb.serial_number.as_deref());
    let message = match mtp_session.status {
        MtpSessionStatus::Open if prepared.switched => {
            "TP-7 switched to MTP mode and the MTP session opened.".to_string()
        }
        MtpSessionStatus::Open => {
            "TP-7 already exposes MTP and the MTP session opened.".to_string()
        }
        MtpSessionStatus::Busy if prepared.switched => {
            "TP-7 switched to MTP mode, but another process owns the MTP interface.".to_string()
        }
        MtpSessionStatus::Busy => {
            "TP-7 exposes MTP, but another process owns the MTP interface.".to_string()
        }
        MtpSessionStatus::Failed if prepared.switched => {
            "TP-7 switched to MTP mode, but MTP session validation failed.".to_string()
        }
        MtpSessionStatus::Failed => {
            "TP-7 exposes MTP, but MTP session validation failed.".to_string()
        }
    };

    ConnectReport {
        serial_number: prepared.usb.serial_number.clone(),
        initial_mode: prepared.initial_usb.mode.clone(),
        final_mode: prepared.usb.mode.clone(),
        mtp_ready: is_mtp_visible(&prepared.usb),
        switched: prepared.switched,
        midi_switch: prepared.midi_switch.clone(),
        mtp_session,
        message,
    }
}

fn validate_mtp_session(serial: Option<&str>) -> MtpSessionCheck {
    match block_on(async {
        let device = open_mtp_device(serial).await?;
        let storages = device
            .storages()
            .await
            .map_err(crate::mtp_session::map_mtp_error)?;
        let storage_count = storages.len();
        release_mtp_device(device);
        Ok(storage_count)
    }) {
        Ok(storage_count) => MtpSessionCheck {
            status: MtpSessionStatus::Open,
            storage_count: Some(storage_count),
            message: "MTP session opened successfully.".to_string(),
        },
        Err(AppError::MtpExclusiveAccess { message }) => MtpSessionCheck {
            status: MtpSessionStatus::Busy,
            storage_count: None,
            message,
        },
        Err(error) => MtpSessionCheck {
            status: MtpSessionStatus::Failed,
            storage_count: None,
            message: error.to_string(),
        },
    }
}

fn is_mtp_visible(device: &Tp7Device) -> bool {
    matches!(device.mode, UsbMode::Mtp | UsbMode::Mixed)
}
