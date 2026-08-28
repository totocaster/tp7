use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, open_mtp_session_with_takeover};
use crate::output::AppError;
use crate::remote::{RemoteObject, RemoteTarget, first_storage, list_remote_objects, resolve_path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LsReport {
    pub path: String,
    pub storage_id: u64,
    pub entries: Vec<LsEntry>,
}

pub type LsEntry = RemoteObject;

pub fn run_ls(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
    take_over: bool,
) -> Result<LsReport, AppError> {
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session_with_takeover(serial, policy, take_over).await?;
        match read_ls(&session.device, remote_path).await {
            Ok(report) => {
                session.release().await?;
                Ok(report)
            }
            Err(error) => {
                let _ = session.release().await;
                Err(error)
            }
        }
    })
}

async fn read_ls(device: &mtp_rs::MtpDevice, remote_path: &str) -> Result<LsReport, AppError> {
    let storage = first_storage(device).await?;
    let resolved = resolve_path(&storage, remote_path).await?;
    let entries = match resolved.target {
        RemoteTarget::Root => list_remote_objects(&storage, None).await?,
        RemoteTarget::Object(object) if object.is_folder() => {
            list_remote_objects(&storage, Some(object.handle)).await?
        }
        RemoteTarget::Object(object) => vec![LsEntry::from_object(object)],
    };

    Ok(LsReport {
        path: resolved.path,
        storage_id: storage.id().0,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use crate::remote::ObjectKind;

    use super::*;

    #[test]
    fn parses_root_listing_fixture() {
        let fixture = include_str!("../tests/fixtures/tp7-root-listing.json");
        let report = serde_json::from_str::<LsReport>(fixture).unwrap();

        assert_eq!(report.path, "/");
        assert_eq!(report.storage_id, 65_537);
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].kind, ObjectKind::Folder);
    }
}
