use mtp_rs::{ObjectHandle, ObjectInfo, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, open_mtp_session};
use crate::output::AppError;
use crate::remote::join_remote_path;
use crate::remote::{RemoteObject, RemoteTarget, first_storage, list_object_infos, resolve_path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeReport {
    pub path: String,
    pub storage_id: u64,
    pub entries: Vec<TreeEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeEntry {
    pub path: String,
    pub depth: usize,
    pub object: RemoteObject,
}

pub fn run_tree(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
    depth: Option<usize>,
) -> Result<TreeReport, AppError> {
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = read_tree(&session.device, remote_path, depth).await;
        let release_result = session.release().await;

        match (result, release_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

async fn read_tree(
    device: &mtp_rs::MtpDevice,
    remote_path: &str,
    depth: Option<usize>,
) -> Result<TreeReport, AppError> {
    let storage = first_storage(device).await?;
    let resolved = resolve_path(&storage, remote_path).await?;
    let mut entries = Vec::new();

    match resolved.target {
        RemoteTarget::Root => {
            collect_children(&storage, None, &resolved.path, 0, depth, &mut entries).await?;
        }
        RemoteTarget::Object(object) if object.is_folder() => {
            collect_children(
                &storage,
                Some(object.handle),
                &resolved.path,
                0,
                depth,
                &mut entries,
            )
            .await?;
        }
        RemoteTarget::Object(object) => {
            entries.push(TreeEntry::from_object(resolved.path.clone(), 0, object));
        }
    }

    Ok(TreeReport {
        path: resolved.path,
        storage_id: storage.id().0,
        entries,
    })
}

async fn collect_children(
    storage: &Storage,
    parent: Option<ObjectHandle>,
    parent_path: &str,
    depth: usize,
    max_depth: Option<usize>,
    entries: &mut Vec<TreeEntry>,
) -> Result<(), AppError> {
    if max_depth.is_some_and(|max_depth| depth >= max_depth) {
        return Ok(());
    }

    let mut stack = list_object_infos(storage, parent)
        .await?
        .into_iter()
        .rev()
        .map(|object| (object, depth, parent_path.to_string()))
        .collect::<Vec<_>>();

    while let Some((object, current_depth, current_parent_path)) = stack.pop() {
        let path = join_remote_path(&current_parent_path, &object.filename);
        let should_descend =
            object.is_folder() && max_depth.is_none_or(|max_depth| current_depth + 1 < max_depth);
        let child_parent = object.handle;

        entries.push(TreeEntry::from_object(path.clone(), current_depth, object));

        if should_descend {
            let children = list_object_infos(storage, Some(child_parent)).await?;
            stack.extend(
                children
                    .into_iter()
                    .rev()
                    .map(|child| (child, current_depth + 1, path.clone())),
            );
        }
    }

    Ok(())
}

impl TreeEntry {
    fn from_object(path: String, depth: usize, object: ObjectInfo) -> Self {
        Self {
            path,
            depth,
            object: RemoteObject::from_object(object),
        }
    }
}

#[cfg(test)]
mod tests {
    use mtp_rs::{ObjectFormat, ObjectHandle, ObjectInfo};

    use crate::remote::ObjectKind;

    use super::*;

    #[test]
    fn builds_tree_entry_from_object() {
        let mut object = ObjectInfo::default();
        object.handle = ObjectHandle(16);
        object.format = ObjectFormat(0x3008);
        object.filename = "take.wav".to_string();
        object.size = 1024;

        let entry = TreeEntry::from_object("/recordings/take.wav".to_string(), 1, object);

        assert_eq!(entry.path, "/recordings/take.wav");
        assert_eq!(entry.depth, 1);
        assert_eq!(entry.object.name, "take.wav");
        assert_eq!(entry.object.kind, ObjectKind::File);
    }
}
