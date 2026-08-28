use serde::Serialize;

use crate::device::UsbMode;
use crate::mtp_session::{MtpOpenPolicy, block_on, open_mtp_session};
use crate::output::AppError;

#[derive(Debug, Clone, Serialize)]
pub struct EjectReport {
    pub serial_number: Option<String>,
    pub initial_mode: UsbMode,
    pub final_mode: UsbMode,
    pub switched: bool,
    pub closed: bool,
    pub message: String,
}

pub fn run_eject(serial: Option<&str>, auto_connect: bool) -> Result<EjectReport, AppError> {
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let report = EjectReport {
            serial_number: session.prepared.usb.serial_number.clone(),
            initial_mode: session.prepared.initial_usb.mode.clone(),
            final_mode: session.prepared.usb.mode.clone(),
            switched: session.prepared.switched,
            closed: true,
            message: "MTP session opened and closed cleanly.".to_string(),
        };

        session.eject().await?;
        Ok(report)
    })
}
