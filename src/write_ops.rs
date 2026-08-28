use mtp_rs::{ObjectHandle, ObjectInfo, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, map_mtp_error, open_mtp_session};
use crate::output::AppError;
use crate::remote::{
    RemoteObject, RemoteTarget, first_storage, format_path, join_remote_path, list_object_infos,
    path_components, resolve_path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MkdirReport {
    pub path: String,
    pub created: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameReport {
    pub old_path: String,
    pub new_path: String,
    pub object: RemoteObject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RmReport {
    pub path: String,
    pub dry_run: bool,
    pub removed: Vec<RemoteObject>,
}

pub fn run_mkdir(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
    parents: bool,
) -> Result<MkdirReport, AppError> {
    let policy = open_policy(auto_connect);

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = write_mkdir(&session.device, remote_path, parents).await;
        let release_result = session.release().await;

        match (result, release_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

pub fn run_rename(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
    new_name: &str,
) -> Result<RenameReport, AppError> {
    validate_remote_name(new_name)?;
    let policy = open_policy(auto_connect);

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = write_rename(&session.device, remote_path, new_name).await;
        let release_result = session.release().await;

        match (result, release_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

pub fn run_rm(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
    recursive: bool,
    force: bool,
    dry_run: bool,
) -> Result<RmReport, AppError> {
    let policy = open_policy(auto_connect);

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = write_rm(&session.device, remote_path, recursive, force, dry_run).await;
        let release_result = session.release().await;

        match (result, release_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

async fn write_mkdir(
    device: &mtp_rs::MtpDevice,
    remote_path: &str,
    parents: bool,
) -> Result<MkdirReport, AppError> {
    let storage = first_storage(device).await?;
    let components = path_components(remote_path)?;
    if components.is_empty() {
        return Err(AppError::RemotePathIsRoot);
    }

    let path = format_path(&components);
    let mut parent = None;
    let mut current_path = "/".to_string();
    let mut created = Vec::new();

    for (index, component) in components.iter().enumerate() {
        validate_remote_name(component)?;
        let child_path = join_remote_path(&current_path, component);
        let existing = find_child(&storage, parent, component).await?;

        match existing {
            Some(existing) if existing.is_folder() => {
                if index == components.len() - 1 && !parents {
                    return Err(AppError::RemotePathExists { path: child_path });
                }
                parent = Some(existing.handle);
                current_path = child_path;
            }
            Some(_) => return Err(AppError::RemotePathExists { path: child_path }),
            None => {
                if !parents && index != components.len() - 1 {
                    return Err(AppError::RemotePathNotFound { path: child_path });
                }

                let handle = storage
                    .create_folder(parent, component)
                    .await
                    .map_err(map_create_folder_error)?;
                created.push(child_path.clone());
                parent = Some(handle);
                current_path = child_path;
            }
        }
    }

    Ok(MkdirReport { path, created })
}

async fn write_rename(
    device: &mtp_rs::MtpDevice,
    remote_path: &str,
    new_name: &str,
) -> Result<RenameReport, AppError> {
    let storage = first_storage(device).await?;
    let resolved = resolve_path(&storage, remote_path).await?;
    let RemoteTarget::Object(object) = resolved.target else {
        return Err(AppError::RemotePathIsRoot);
    };
    let old_path = resolved.path;
    let new_path = renamed_path(&old_path, new_name);

    if find_child(&storage, parent_handle(object.parent), new_name)
        .await?
        .is_some()
    {
        return Err(AppError::RemotePathExists {
            path: new_path.clone(),
        });
    }

    storage
        .rename(object.handle, new_name)
        .await
        .map_err(map_mtp_error)?;

    let mut renamed = object;
    renamed.filename = new_name.to_string();

    Ok(RenameReport {
        old_path,
        new_path,
        object: RemoteObject::from_object(renamed),
    })
}

async fn write_rm(
    device: &mtp_rs::MtpDevice,
    remote_path: &str,
    recursive: bool,
    force: bool,
    dry_run: bool,
) -> Result<RmReport, AppError> {
    let storage = first_storage(device).await?;
    let resolved = match resolve_path(&storage, remote_path).await {
        Ok(resolved) => resolved,
        Err(AppError::RemotePathNotFound { .. }) if force => {
            return Ok(RmReport {
                path: remote_path.to_string(),
                dry_run,
                removed: Vec::new(),
            });
        }
        Err(error) => return Err(error),
    };
    let RemoteTarget::Object(object) = resolved.target else {
        return Err(AppError::RemotePathIsRoot);
    };

    if object.is_folder() && !recursive {
        return Err(AppError::RemotePathIsFolder {
            path: resolved.path,
        });
    }

    let removed = vec![RemoteObject::from_object(object.clone())];
    if !dry_run {
        storage.delete(object.handle).await.map_err(map_mtp_error)?;
    }

    Ok(RmReport {
        path: resolved.path,
        dry_run,
        removed,
    })
}

async fn find_child(
    storage: &Storage,
    parent: Option<ObjectHandle>,
    name: &str,
) -> Result<Option<ObjectInfo>, AppError> {
    Ok(list_object_infos(storage, parent)
        .await?
        .into_iter()
        .find(|object| object.filename == name))
}

fn open_policy(auto_connect: bool) -> MtpOpenPolicy {
    if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    }
}

fn validate_remote_name(name: &str) -> Result<(), AppError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(AppError::InvalidRemotePath {
            path: name.to_string(),
            message: "remote object names cannot contain path separators".to_string(),
        });
    }

    Ok(())
}

fn parent_handle(parent: ObjectHandle) -> Option<ObjectHandle> {
    if parent.0 == 0 { None } else { Some(parent) }
}

fn renamed_path(old_path: &str, new_name: &str) -> String {
    let parent =
        old_path.rsplit_once('/').map_or(
            "/",
            |(parent, _)| {
                if parent.is_empty() { "/" } else { parent }
            },
        );

    join_remote_path(parent, new_name)
}

fn map_create_folder_error(error: mtp_rs::Error) -> AppError {
    match error {
        mtp_rs::Error::Unsupported | mtp_rs::Error::AccessDenied | mtp_rs::Error::Other { .. } => {
            AppError::MtpUnsupported {
                message: format!("TP-7 rejected folder creation ({error})"),
            }
        }
        _ => map_mtp_error(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_remote_names() {
        assert!(validate_remote_name("../x").is_err());
        assert!(validate_remote_name("").is_err());
    }

    #[test]
    fn maps_tp7_general_folder_error_to_unsupported() {
        let error = mtp_rs::Error::Other {
            detail: "GeneralError".to_string(),
        };

        assert!(matches!(
            map_create_folder_error(error),
            AppError::MtpUnsupported { .. }
        ));
    }

    #[test]
    fn builds_renamed_root_child_path() {
        assert_eq!(renamed_path("/old.txt", "new.txt"), "/new.txt");
    }

    #[test]
    fn builds_renamed_nested_path() {
        assert_eq!(renamed_path("/memo/old.txt", "new.txt"), "/memo/new.txt");
    }
}
