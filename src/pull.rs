use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use mtp_rs::{ByteRange, DEFAULT_CANCEL_TIMEOUT, ObjectHandle, ObjectInfo, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, block_on, map_mtp_error, open_mtp_session};
use crate::output::AppError;
use crate::remote::{
    RemoteTarget, first_storage, join_remote_path, list_object_infos, resolve_path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullReport {
    pub remote_path: String,
    pub local_path: String,
    pub dry_run: bool,
    pub downloaded: usize,
    pub skipped: usize,
    pub total_bytes: u64,
    pub files: Vec<PullFileReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PullFileReport {
    pub remote_path: String,
    pub local_path: String,
    pub size: u64,
    pub status: PullStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PullStatus {
    Downloaded,
    DryRun,
    Skipped,
}

#[derive(Debug, Clone, Copy)]
pub struct PullOptions {
    pub recursive: bool,
    pub overwrite: bool,
    pub skip_existing: bool,
    pub dry_run: bool,
    pub progress: bool,
}

pub fn run_pull(
    serial: Option<&str>,
    auto_connect: bool,
    remote_path: &str,
    local_path: Option<&str>,
    options: PullOptions,
) -> Result<PullReport, AppError> {
    validate_options(options)?;
    let policy = if auto_connect {
        MtpOpenPolicy::AutoSwitch
    } else {
        MtpOpenPolicy::RequireAutoConnectFlag
    };

    block_on(async {
        let session = open_mtp_session(serial, policy).await?;
        let result = read_pull(&session.device, remote_path, local_path, options).await;
        let release_result = session.release().await;

        match (result, release_result) {
            (Ok(report), Ok(())) => Ok(report),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    })
}

async fn read_pull(
    device: &mtp_rs::MtpDevice,
    remote_path: &str,
    local_path: Option<&str>,
    options: PullOptions,
) -> Result<PullReport, AppError> {
    let storage = first_storage(device).await?;
    let resolved = resolve_path(&storage, remote_path).await?;
    let mut report = PullReport {
        remote_path: resolved.path.clone(),
        local_path: String::new(),
        dry_run: options.dry_run,
        downloaded: 0,
        skipped: 0,
        total_bytes: 0,
        files: Vec::new(),
    };

    match resolved.target {
        RemoteTarget::Root => Err(AppError::RemotePathIsRoot),
        RemoteTarget::Object(object) if object.is_folder() => {
            if !options.recursive {
                return Err(AppError::RemotePathIsFolder {
                    path: resolved.path,
                });
            }

            let destination = folder_destination(local_path, &object.filename)?;
            report.local_path = path_to_string(&destination);
            pull_folder(
                &storage,
                object.handle,
                &resolved.path,
                &destination,
                options,
                &mut report,
            )
            .await?;
            Ok(report)
        }
        RemoteTarget::Object(object) => {
            let destination = file_destination(local_path, &object.filename)?;
            report.local_path = path_to_string(&destination);
            pull_file(
                &storage,
                object,
                &resolved.path,
                &destination,
                options,
                &mut report,
            )
            .await?;
            Ok(report)
        }
    }
}

async fn pull_folder(
    storage: &Storage,
    root_handle: ObjectHandle,
    root_remote_path: &str,
    root_local_path: &Path,
    options: PullOptions,
    report: &mut PullReport,
) -> Result<(), AppError> {
    if !options.dry_run {
        create_dir_all(root_local_path)?;
    }

    let mut stack = list_object_infos(storage, Some(root_handle))
        .await?
        .into_iter()
        .rev()
        .map(|object| {
            (
                object,
                root_remote_path.to_string(),
                root_local_path.to_path_buf(),
            )
        })
        .collect::<Vec<_>>();

    while let Some((object, remote_parent_path, local_parent_path)) = stack.pop() {
        let remote_path = join_remote_path(&remote_parent_path, &object.filename);
        let local_path = safe_child_path(&local_parent_path, &object.filename)?;

        if object.is_folder() {
            if !options.dry_run {
                create_dir_all(&local_path)?;
            }

            let children = list_object_infos(storage, Some(object.handle)).await?;
            stack.extend(
                children
                    .into_iter()
                    .rev()
                    .map(|child| (child, remote_path.clone(), local_path.clone())),
            );
        } else {
            pull_file(storage, object, &remote_path, &local_path, options, report).await?;
        }
    }

    Ok(())
}

async fn pull_file(
    storage: &Storage,
    object: ObjectInfo,
    remote_path: &str,
    local_path: &Path,
    options: PullOptions,
    report: &mut PullReport,
) -> Result<(), AppError> {
    let status = prepare_file_destination(local_path, options)?;
    match status {
        PullStatus::Skipped => {
            report.skipped += 1;
            report.files.push(file_report(
                remote_path,
                local_path,
                object.size,
                PullStatus::Skipped,
            ));
            Ok(())
        }
        PullStatus::DryRun => {
            report.files.push(file_report(
                remote_path,
                local_path,
                object.size,
                PullStatus::DryRun,
            ));
            Ok(())
        }
        PullStatus::Downloaded => {
            let temp_path = temp_download_path(local_path)?;
            if let Err(error) =
                download_to_path(storage, object.handle, object.size, &temp_path, options).await
            {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            }
            if let Err(error) = rename_file(&temp_path, local_path) {
                let _ = fs::remove_file(&temp_path);
                return Err(error);
            }
            if let Err(error) = verify_file_size(local_path, object.size) {
                let _ = fs::remove_file(local_path);
                return Err(error);
            }

            report.downloaded += 1;
            report.total_bytes += object.size;
            report.files.push(file_report(
                remote_path,
                local_path,
                object.size,
                PullStatus::Downloaded,
            ));
            Ok(())
        }
    }
}

async fn download_to_path(
    storage: &Storage,
    handle: ObjectHandle,
    expected_size: u64,
    local_path: &Path,
    options: PullOptions,
) -> Result<(), AppError> {
    let mut download = storage
        .download(handle, ByteRange::Full)
        .await
        .map_err(map_mtp_error)?;
    let mut file = create_file(local_path)?;

    while let Some(chunk) = download.next_chunk().await {
        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(error) => {
                let _ = download.cancel(DEFAULT_CANCEL_TIMEOUT).await;
                finish_progress(options.progress);
                return Err(map_mtp_error(error));
            }
        };
        if let Err(error) = file.write_all(&bytes) {
            let _ = download.cancel(DEFAULT_CANCEL_TIMEOUT).await;
            finish_progress(options.progress);
            return Err(io_error(local_path, error));
        }
        write_progress(options.progress, download.bytes_received(), expected_size);
    }

    finish_progress(options.progress);
    file.flush().map_err(|error| io_error(local_path, error))?;
    Ok(())
}

fn prepare_file_destination(
    local_path: &Path,
    options: PullOptions,
) -> Result<PullStatus, AppError> {
    if local_path.exists() {
        if local_path.is_dir() {
            return Err(AppError::LocalPathIsDirectory {
                path: path_to_string(local_path),
            });
        }

        if options.skip_existing {
            return Ok(PullStatus::Skipped);
        }

        if !options.overwrite {
            return Err(AppError::LocalPathExists {
                path: path_to_string(local_path),
            });
        }
    }

    if let Some(parent) = local_path.parent()
        && !parent.as_os_str().is_empty()
        && !options.dry_run
    {
        create_dir_all(parent)?;
    }

    if options.dry_run {
        Ok(PullStatus::DryRun)
    } else {
        Ok(PullStatus::Downloaded)
    }
}

fn file_destination(local_path: Option<&str>, remote_name: &str) -> Result<PathBuf, AppError> {
    let Some(local_path) = local_path else {
        return Ok(PathBuf::from(remote_name));
    };
    let local_path = PathBuf::from(local_path);

    if local_path.is_dir() || local_path_looks_like_directory(local_path.as_path()) {
        return safe_child_path(&local_path, remote_name);
    }

    Ok(local_path)
}

fn folder_destination(local_path: Option<&str>, remote_name: &str) -> Result<PathBuf, AppError> {
    match local_path {
        Some(local_path) => {
            let local_path = PathBuf::from(local_path);
            if local_path.exists() && !local_path.is_dir() {
                return Err(AppError::LocalPathIsFile {
                    path: path_to_string(&local_path),
                });
            }
            Ok(local_path)
        }
        None => Ok(PathBuf::from(remote_name)),
    }
}

fn safe_child_path(parent: &Path, name: &str) -> Result<PathBuf, AppError> {
    if name == "." || name == ".." || name.contains('/') {
        return Err(AppError::InvalidRemotePath {
            path: name.to_string(),
            message: "remote object names cannot contain path separators".to_string(),
        });
    }

    Ok(parent.join(name))
}

fn temp_download_path(local_path: &Path) -> Result<PathBuf, AppError> {
    let Some(file_name) = local_path.file_name() else {
        return Err(AppError::InvalidArguments {
            message: "local path must include a file name".to_string(),
        });
    };
    let temp_name = format!(".{}.tp7tmp", file_name.to_string_lossy());

    Ok(local_path.with_file_name(temp_name))
}

fn validate_options(options: PullOptions) -> Result<(), AppError> {
    if options.overwrite && options.skip_existing {
        return Err(AppError::InvalidArguments {
            message: "--overwrite and --skip-existing cannot be used together".to_string(),
        });
    }

    Ok(())
}

fn write_progress(enabled: bool, transferred: u64, total: u64) {
    if !enabled {
        return;
    }

    let percent = if total == 0 {
        100.0
    } else {
        transferred as f64 / total as f64 * 100.0
    };

    eprint!("\rdownloaded {transferred}/{total} bytes ({percent:.1}%)");
    let _ = io::stderr().flush();
}

fn finish_progress(enabled: bool) {
    if enabled {
        eprintln!();
    }
}

fn local_path_looks_like_directory(path: &Path) -> bool {
    path.to_string_lossy().ends_with('/')
}

fn file_report(
    remote_path: &str,
    local_path: &Path,
    size: u64,
    status: PullStatus,
) -> PullFileReport {
    PullFileReport {
        remote_path: remote_path.to_string(),
        local_path: path_to_string(local_path),
        size,
        status,
    }
}

fn create_file(path: &Path) -> Result<File, AppError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_dir_all(parent)?;
    }

    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(|error| io_error(path, error))
}

fn create_dir_all(path: &Path) -> Result<(), AppError> {
    fs::create_dir_all(path).map_err(|error| io_error(path, error))
}

fn rename_file(from: &Path, to: &Path) -> Result<(), AppError> {
    fs::rename(from, to).map_err(|error| io_error(to, error))
}

fn verify_file_size(path: &Path, expected_size: u64) -> Result<(), AppError> {
    let actual_size = fs::metadata(path)
        .map_err(|error| io_error(path, error))?
        .len();

    if actual_size != expected_size {
        return Err(AppError::TransferVerification {
            path: path_to_string(path),
            expected_size,
            actual_size,
        });
    }

    Ok(())
}

fn io_error(path: &Path, error: std::io::Error) -> AppError {
    AppError::FileSystem {
        path: path_to_string(path),
        message: error.to_string(),
    }
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_file_destination_to_remote_name() {
        assert_eq!(
            file_destination(None, "take.wav").unwrap(),
            PathBuf::from("take.wav")
        );
    }

    #[test]
    fn treats_trailing_slash_file_destination_as_directory() {
        assert_eq!(
            file_destination(Some("out/"), "take.wav").unwrap(),
            PathBuf::from("out/take.wav")
        );
    }

    #[test]
    fn rejects_unsafe_child_names() {
        let error = safe_child_path(Path::new("out"), "../take.wav").unwrap_err();

        assert!(matches!(error, AppError::InvalidRemotePath { .. }));
    }

    #[test]
    fn builds_temp_download_path_next_to_destination() {
        assert_eq!(
            temp_download_path(Path::new("out/take.wav")).unwrap(),
            PathBuf::from("out/.take.wav.tp7tmp")
        );
    }

    #[test]
    fn rejects_conflicting_overwrite_and_skip_options() {
        let error = validate_options(PullOptions {
            recursive: false,
            overwrite: true,
            skip_existing: true,
            dry_run: false,
            progress: false,
        })
        .unwrap_err();

        assert!(matches!(error, AppError::InvalidArguments { .. }));
    }

    #[test]
    fn detects_size_mismatch() {
        let path = Path::new("target/tp7-unit-size-mismatch.tmp");
        fs::write(path, b"abc").unwrap();

        let error = verify_file_size(path, 4).unwrap_err();
        let _ = fs::remove_file(path);

        assert!(matches!(error, AppError::TransferVerification { .. }));
    }
}
