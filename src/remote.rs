use mtp_rs::{DateTime, MtpDevice, ObjectHandle, ObjectInfo, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::map_mtp_error;
use crate::output::AppError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteObject {
    pub id: u64,
    pub parent_id: u64,
    pub storage_id: u64,
    pub kind: ObjectKind,
    pub name: String,
    pub size: u64,
    pub modified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ObjectKind {
    File,
    Folder,
}

pub struct ResolvedPath {
    pub path: String,
    pub target: RemoteTarget,
}

pub enum RemoteTarget {
    Root,
    Object(ObjectInfo),
}

impl RemoteObject {
    pub fn from_object(object: ObjectInfo) -> Self {
        Self {
            id: object.handle.0,
            parent_id: object.parent.0,
            storage_id: object.storage_id.0,
            kind: if object.is_folder() {
                ObjectKind::Folder
            } else {
                ObjectKind::File
            },
            name: object.filename,
            size: object.size,
            modified: object.modified.map(format_datetime),
        }
    }
}

pub fn format_datetime(datetime: DateTime) -> String {
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}",
        datetime.year,
        datetime.month,
        datetime.day,
        datetime.hour,
        datetime.minute,
        datetime.second
    )
}

pub async fn first_storage(device: &MtpDevice) -> Result<Storage, AppError> {
    let mut storages = device.storages().await.map_err(map_mtp_error)?;

    if storages.is_empty() {
        return Err(AppError::Mtp {
            message: "TP-7 reported no MTP storages.".to_string(),
        });
    }

    Ok(storages.remove(0))
}

pub async fn resolve_path(storage: &Storage, path: &str) -> Result<ResolvedPath, AppError> {
    let path = normalize_remote_path(path)?;
    let components = path_components(&path)?;

    if components.is_empty() {
        return Ok(ResolvedPath {
            path,
            target: RemoteTarget::Root,
        });
    }

    let mut parent = None;

    for (index, component) in components.iter().enumerate() {
        let children = list_object_infos(storage, parent).await?;
        let object = children
            .into_iter()
            .find(|object| object.filename == *component)
            .ok_or_else(|| AppError::RemotePathNotFound {
                path: format_path(&components[..=index]),
            })?;

        if index == components.len() - 1 {
            return Ok(ResolvedPath {
                path,
                target: RemoteTarget::Object(object),
            });
        }

        if !object.is_folder() {
            return Err(AppError::RemotePathNotDirectory {
                path: format_path(&components[..=index]),
            });
        }

        parent = Some(object.handle);
    }

    Ok(ResolvedPath {
        path,
        target: RemoteTarget::Root,
    })
}

pub async fn list_object_infos(
    storage: &Storage,
    parent: Option<ObjectHandle>,
) -> Result<Vec<ObjectInfo>, AppError> {
    let mut objects = storage.list_objects(parent).await.map_err(map_mtp_error)?;
    sort_object_infos(&mut objects);
    Ok(objects)
}

pub async fn list_remote_objects(
    storage: &Storage,
    parent: Option<ObjectHandle>,
) -> Result<Vec<RemoteObject>, AppError> {
    let mut objects = list_object_infos(storage, parent)
        .await?
        .into_iter()
        .map(RemoteObject::from_object)
        .collect::<Vec<_>>();

    sort_remote_objects(&mut objects);

    Ok(objects)
}

pub fn normalize_remote_path(path: &str) -> Result<String, AppError> {
    let components = path_components(path)?;
    Ok(format_path(&components))
}

pub fn path_components(path: &str) -> Result<Vec<String>, AppError> {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return Ok(Vec::new());
    }

    let components = trimmed
        .trim_matches('/')
        .split('/')
        .filter(|component| !component.is_empty())
        .map(|component| {
            if component == "." || component == ".." {
                return Err(AppError::InvalidRemotePath {
                    path: path.to_string(),
                    message: "remote paths cannot contain . or .. components".to_string(),
                });
            }

            Ok(component.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(components)
}

pub fn format_path(components: &[String]) -> String {
    if components.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", components.join("/"))
    }
}

pub fn join_remote_path(parent_path: &str, name: &str) -> String {
    if parent_path == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent_path.trim_end_matches('/'), name)
    }
}

pub fn sort_remote_objects(objects: &mut [RemoteObject]) {
    objects.sort_by(|left, right| {
        let left_kind = kind_sort_key(&left.kind);
        let right_kind = kind_sort_key(&right.kind);

        left_kind
            .cmp(&right_kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}

pub fn sort_object_infos(objects: &mut [ObjectInfo]) {
    objects.sort_by(|left, right| {
        let left_kind = if left.is_folder() { 0 } else { 1 };
        let right_kind = if right.is_folder() { 0 } else { 1 };

        left_kind
            .cmp(&right_kind)
            .then_with(|| {
                left.filename
                    .to_lowercase()
                    .cmp(&right.filename.to_lowercase())
            })
            .then_with(|| left.filename.cmp(&right.filename))
    });
}

fn kind_sort_key(kind: &ObjectKind) -> u8 {
    match kind {
        ObjectKind::Folder => 0,
        ObjectKind::File => 1,
    }
}

#[cfg(test)]
mod tests {
    use mtp_rs::{DateTime, ObjectFormat, ObjectHandle, ObjectInfo, StorageId};

    use super::*;

    #[test]
    fn normalizes_empty_and_root_paths_to_root() {
        assert_eq!(normalize_remote_path("").unwrap(), "/");
        assert_eq!(normalize_remote_path("/").unwrap(), "/");
    }

    #[test]
    fn normalizes_repeated_separators() {
        assert_eq!(
            normalize_remote_path("//Recordings//Take 1//").unwrap(),
            "/Recordings/Take 1"
        );
    }

    #[test]
    fn rejects_parent_path_components() {
        let error = normalize_remote_path("/Recordings/../System").unwrap_err();

        assert!(matches!(error, AppError::InvalidRemotePath { .. }));
    }

    #[test]
    fn converts_object_info_to_remote_object() {
        let mut object = ObjectInfo::default();
        object.handle = ObjectHandle(42);
        object.storage_id = StorageId(65_537);
        object.parent = ObjectHandle(7);
        object.filename = "take.wav".to_string();
        object.size = 1024;
        object.modified = Some(DateTime {
            year: 2026,
            month: 5,
            day: 7,
            hour: 12,
            minute: 30,
            second: 45,
        });

        let entry = RemoteObject::from_object(object);

        assert_eq!(entry.id, 42);
        assert_eq!(entry.parent_id, 7);
        assert_eq!(entry.storage_id, 65_537);
        assert_eq!(entry.kind, ObjectKind::File);
        assert_eq!(entry.name, "take.wav");
        assert_eq!(entry.size, 1024);
        assert_eq!(entry.modified.as_deref(), Some("20260507T123045"));
    }

    #[test]
    fn sorts_folders_before_files_by_name() {
        let mut entries = vec![
            entry("z.wav", ObjectKind::File),
            entry("Recordings", ObjectKind::Folder),
            entry("a.wav", ObjectKind::File),
        ];

        sort_remote_objects(&mut entries);

        assert_eq!(
            entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            vec!["Recordings", "a.wav", "z.wav"]
        );
    }

    #[test]
    fn sorts_object_infos_by_name() {
        let mut objects = vec![object("z.wav"), object("Recordings"), object("a.wav")];

        sort_object_infos(&mut objects);

        assert_eq!(
            objects
                .into_iter()
                .map(|object| object.filename)
                .collect::<Vec<_>>(),
            vec!["a.wav", "Recordings", "z.wav"]
        );
    }

    #[test]
    fn joins_remote_root_paths() {
        assert_eq!(join_remote_path("/", "recordings"), "/recordings");
    }

    #[test]
    fn joins_remote_nested_paths() {
        assert_eq!(
            join_remote_path("/recordings", "take.wav"),
            "/recordings/take.wav"
        );
    }

    fn entry(name: &str, kind: ObjectKind) -> RemoteObject {
        RemoteObject {
            id: 0,
            parent_id: 0,
            storage_id: 0,
            kind,
            name: name.to_string(),
            size: 0,
            modified: None,
        }
    }

    fn object(name: &str) -> ObjectInfo {
        let mut object = ObjectInfo::default();
        object.filename = name.to_string();
        object.format = ObjectFormat(0x3008);
        object
    }
}
