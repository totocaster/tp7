use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mtp_rs::ptp::{ObjectHandle, ObjectInfo, OperationCode, ResponseCode};
use mtp_rs::{DEFAULT_CANCEL_TIMEOUT, Storage};
use serde::{Deserialize, Serialize};

use crate::mtp_session::{MtpOpenPolicy, open_mtp_session};
use crate::output::AppError;
use crate::remote::{
    RemoteTarget, first_storage, join_remote_path, list_object_infos, normalize_remote_path,
    resolve_path,
};

const DEFAULT_VOLUME_NAME: &str = "TP-7";
const DEFAULT_MOUNT_ROOT: &str = "/Volumes";
const SHUTDOWN_PATH: &str = "/.tp7-shutdown";
const PARTIAL_CHUNK_SIZE: u64 = 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct MountReport {
    pub mountpoint: String,
    pub url: String,
    pub pid: u32,
    pub serial_number: Option<String>,
    pub switched: bool,
    pub read_write: bool,
    pub partial_reads: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmountReport {
    pub requested_mountpoint: Option<String>,
    pub unmounted: Vec<UnmountedMount>,
    pub failed: Vec<UnmountFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmountedMount {
    pub mountpoint: String,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnmountFailure {
    pub mountpoint: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MountState {
    mountpoint: String,
    url: String,
    port: u16,
    token: String,
    pid: u32,
    serial_number: Option<String>,
    read_write: bool,
}

struct ServerReady {
    serial_number: Option<String>,
    switched: bool,
    partial_reads: bool,
}

pub struct MountGuard {
    pub report: MountReport,
    running: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<Result<(), AppError>>>,
    mountpoint: PathBuf,
    state_path: PathBuf,
}

impl MountGuard {
    pub fn wait(mut self) -> Result<(), AppError> {
        while self.running.load(Ordering::SeqCst) && is_mounted(&self.mountpoint) {
            thread::sleep(Duration::from_secs(1));
        }

        self.running.store(false, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            match thread.join() {
                Ok(result) => result?,
                Err(_) => {
                    return Err(AppError::Runtime {
                        message: "mount server thread panicked".to_string(),
                    });
                }
            }
        }

        remove_state_file(&self.state_path);
        Ok(())
    }
}

pub fn run_mount(
    serial: Option<&str>,
    mountpoint: Option<&str>,
    read_write: bool,
) -> Result<MountGuard, AppError> {
    if read_write {
        return Err(AppError::MtpUnsupported {
            message: "read-write mounts need staged whole-file writes and are not available yet"
                .to_string(),
        });
    }

    if !cfg!(target_os = "macos") {
        return Err(AppError::NotImplemented {
            command: "mount is currently implemented through macOS mount_webdav".to_string(),
        });
    }

    let mountpoint = prepare_mountpoint(mountpoint)?;
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| AppError::Mount {
        message: format!("could not bind local WebDAV server: {error}"),
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|error| AppError::Mount {
            message: format!("could not configure local WebDAV server: {error}"),
        })?;

    let port = listener
        .local_addr()
        .map_err(|error| AppError::Mount {
            message: format!("could not read local WebDAV address: {error}"),
        })?
        .port();
    let token = mount_token();
    let url = format!("http://127.0.0.1:{port}/{token}/");
    let running = Arc::new(AtomicBool::new(true));
    let (ready_tx, ready_rx) = mpsc::channel();
    let server_running = Arc::clone(&running);
    let server_token = token.clone();
    let serial = serial.map(str::to_string);

    let thread = thread::spawn(move || {
        serve_mount(
            listener,
            server_token,
            serial.as_deref(),
            server_running,
            ready_tx,
        )
    });

    let ready = match ready_rx.recv() {
        Ok(Ok(ready)) => ready,
        Ok(Err(error)) => {
            running.store(false, Ordering::SeqCst);
            let _ = thread.join();
            return Err(error);
        }
        Err(error) => {
            running.store(false, Ordering::SeqCst);
            let _ = thread.join();
            return Err(AppError::Mount {
                message: format!("mount server stopped before it was ready: {error}"),
            });
        }
    };

    if let Err(error) = mount_webdav(&url, &mountpoint) {
        running.store(false, Ordering::SeqCst);
        let _ = thread.join();
        return Err(error);
    }

    let state = MountState {
        mountpoint: path_to_string(&mountpoint),
        url: url.clone(),
        port,
        token,
        pid: std::process::id(),
        serial_number: ready.serial_number.clone(),
        read_write,
    };
    let state_path = write_mount_state(&state)?;

    Ok(MountGuard {
        report: MountReport {
            mountpoint: state.mountpoint,
            url,
            pid: state.pid,
            serial_number: ready.serial_number,
            switched: ready.switched,
            read_write,
            partial_reads: ready.partial_reads,
        },
        running,
        thread: Some(thread),
        mountpoint,
        state_path,
    })
}

pub fn run_unmount(mountpoint: Option<&str>) -> Result<UnmountReport, AppError> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::NotImplemented {
            command: "unmount is currently implemented for macOS".to_string(),
        });
    }

    let states = read_mount_states()?;
    let targets = unmount_targets(mountpoint, states)?;
    let mut report = UnmountReport {
        requested_mountpoint: mountpoint.map(str::to_string),
        unmounted: Vec::new(),
        failed: Vec::new(),
    };

    for target in targets {
        match unmount_target(&target.mountpoint, target.state.as_ref()) {
            Ok(()) => {
                report.unmounted.push(UnmountedMount {
                    mountpoint: target.mountpoint.clone(),
                    pid: target.state.as_ref().map(|state| state.state.pid),
                });
                if let Some(state) = target.state {
                    remove_state_file(&state.path);
                }
            }
            Err(error) => {
                report.failed.push(UnmountFailure {
                    mountpoint: target.mountpoint,
                    message: error.to_string(),
                });
            }
        }
    }

    Ok(report)
}

fn serve_mount(
    listener: TcpListener,
    token: String,
    serial: Option<&str>,
    running: Arc<AtomicBool>,
    ready_tx: mpsc::Sender<Result<ServerReady, AppError>>,
) -> Result<(), AppError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| AppError::Runtime {
            message: error.to_string(),
        })?;

    let session = match runtime.block_on(open_mtp_session(serial, MtpOpenPolicy::AutoSwitch)) {
        Ok(session) => session,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return Ok(());
        }
    };
    let switched = session.prepared.switched;
    let serial_number = session.prepared.usb.serial_number.clone();
    let partial_reads = session
        .device
        .device_info()
        .supports_operation(OperationCode::GetPartialObject64)
        || session
            .device
            .device_info()
            .supports_operation(OperationCode::GetPartialObject);
    let storage = match runtime.block_on(first_storage(&session.device)) {
        Ok(storage) => storage,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return Ok(());
        }
    };

    let _ = ready_tx.send(Ok(ServerReady {
        serial_number,
        switched,
        partial_reads,
    }));

    let mut backend = WebDavBackend {
        runtime: &runtime,
        storage,
        token,
        running: Arc::clone(&running),
        partial64_available: true,
        partial32_available: true,
    };

    while running.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = backend.handle_connection(stream) {
                    log::debug!("WebDAV request failed: {error}");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(AppError::Mount {
                    message: format!("local WebDAV server failed: {error}"),
                });
            }
        }
    }

    drop(backend);
    runtime.block_on(session.close())
}

struct WebDavBackend<'a> {
    runtime: &'a tokio::runtime::Runtime,
    storage: Storage,
    token: String,
    running: Arc<AtomicBool>,
    partial64_available: bool,
    partial32_available: bool,
}

impl WebDavBackend<'_> {
    fn handle_connection(&mut self, stream: TcpStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;

        let mut reader = BufReader::new(stream);
        let Some(request) = read_request(&mut reader)? else {
            return Ok(());
        };
        let mut stream = reader.into_inner();

        match request.method.as_str() {
            "OPTIONS" => self.handle_options(&mut stream),
            "PROPFIND" => self.handle_propfind(&request, &mut stream),
            "HEAD" => self.handle_head_or_get(&request, &mut stream, false),
            "GET" => self.handle_head_or_get(&request, &mut stream, true),
            "PUT" | "DELETE" | "MKCOL" | "MOVE" | "COPY" | "LOCK" | "UNLOCK" | "PROPPATCH" => {
                write_empty_response(
                    &mut stream,
                    405,
                    &[
                        ("Allow", "OPTIONS, PROPFIND, GET, HEAD"),
                        ("DAV", "1"),
                        ("Connection", "close"),
                    ],
                )
            }
            _ => write_empty_response(
                &mut stream,
                405,
                &[
                    ("Allow", "OPTIONS, PROPFIND, GET, HEAD"),
                    ("Connection", "close"),
                ],
            ),
        }
    }

    fn handle_options(&self, stream: &mut TcpStream) -> io::Result<()> {
        write_empty_response(
            stream,
            200,
            &[
                ("DAV", "1"),
                ("MS-Author-Via", "DAV"),
                ("Allow", "OPTIONS, PROPFIND, GET, HEAD"),
                ("Connection", "close"),
            ],
        )
    }

    fn handle_propfind(&mut self, request: &HttpRequest, stream: &mut TcpStream) -> io::Result<()> {
        let remote_path = match self.remote_path(request) {
            PathRequest::Remote(path) => path,
            PathRequest::Shutdown => return self.shutdown(stream),
            PathRequest::NotFound => {
                return write_empty_response(stream, 404, &[("Connection", "close")]);
            }
        };

        let depth = request
            .headers
            .get("depth")
            .map(|depth| depth.as_str())
            .unwrap_or("infinity");
        let include_children = depth != "0";
        let resolved = match self
            .runtime
            .block_on(resolve_path(&self.storage, &remote_path))
        {
            Ok(resolved) => resolved,
            Err(AppError::RemotePathNotFound { .. }) => {
                return write_empty_response(stream, 404, &[("Connection", "close")]);
            }
            Err(error) => return write_server_error(stream, &error),
        };

        let mut responses = Vec::new();
        match resolved.target {
            RemoteTarget::Root => {
                responses.push(DavEntry::root(&self.token));
                if include_children {
                    match self
                        .runtime
                        .block_on(list_object_infos(&self.storage, None))
                    {
                        Ok(children) => {
                            responses.extend(
                                children
                                    .into_iter()
                                    .map(|child| DavEntry::from_object(&self.token, "/", child)),
                            );
                        }
                        Err(error) => return write_server_error(stream, &error),
                    }
                }
            }
            RemoteTarget::Object(object) => {
                let object_is_folder = object.is_folder();
                let object_handle = object.handle;
                responses.push(DavEntry::from_object(
                    &self.token,
                    parent_path(&resolved.path),
                    object,
                ));

                if include_children && object_is_folder {
                    match self
                        .runtime
                        .block_on(list_object_infos(&self.storage, Some(object_handle)))
                    {
                        Ok(children) => {
                            responses.extend(children.into_iter().map(|child| {
                                DavEntry::from_object(&self.token, &resolved.path, child)
                            }));
                        }
                        Err(error) => return write_server_error(stream, &error),
                    }
                }
            }
        }

        let body = propfind_body(&responses);
        write_response(
            stream,
            207,
            &[
                ("DAV", "1"),
                ("Content-Type", "application/xml; charset=utf-8"),
                ("Content-Length", &body.len().to_string()),
                ("Connection", "close"),
            ],
            body.as_bytes(),
        )
    }

    fn handle_head_or_get(
        &mut self,
        request: &HttpRequest,
        stream: &mut TcpStream,
        include_body: bool,
    ) -> io::Result<()> {
        let remote_path = match self.remote_path(request) {
            PathRequest::Remote(path) => path,
            PathRequest::Shutdown => return self.shutdown(stream),
            PathRequest::NotFound => {
                return write_empty_response(stream, 404, &[("Connection", "close")]);
            }
        };

        let resolved = match self
            .runtime
            .block_on(resolve_path(&self.storage, &remote_path))
        {
            Ok(resolved) => resolved,
            Err(AppError::RemotePathNotFound { .. }) => {
                return write_empty_response(stream, 404, &[("Connection", "close")]);
            }
            Err(error) => return write_server_error(stream, &error),
        };

        let RemoteTarget::Object(object) = resolved.target else {
            return write_empty_response(stream, 403, &[("Connection", "close")]);
        };
        if object.is_folder() {
            return write_empty_response(stream, 403, &[("Connection", "close")]);
        }

        let size = object.size;
        let content_type = content_type_for_name(&object.filename);
        let range = match request.headers.get("range") {
            Some(range) => match parse_range_header(range, size) {
                Ok(range) => Some(range),
                Err(()) => {
                    return write_empty_response(
                        stream,
                        416,
                        &[
                            ("Content-Range", &format!("bytes */{size}")),
                            ("Connection", "close"),
                        ],
                    );
                }
            },
            None => None,
        };

        let (status, offset, length, content_range) = match range {
            Some(range) => {
                let length = range.end - range.start + 1;
                (
                    206,
                    range.start,
                    length,
                    Some(format!("bytes {}-{}/{}", range.start, range.end, size)),
                )
            }
            None => (200, 0, size, None),
        };

        let content_length = length.to_string();
        let mut headers = vec![
            ("Accept-Ranges", "bytes"),
            ("Content-Type", content_type),
            ("Content-Length", content_length.as_str()),
            ("Connection", "close"),
        ];
        if let Some(content_range) = content_range.as_deref() {
            headers.push(("Content-Range", content_range));
        }

        write_response_head(stream, status, &headers)?;
        if include_body && length > 0 {
            self.write_file_range(stream, object.handle, offset, length)?;
        }
        Ok(())
    }

    fn shutdown(&self, stream: &mut TcpStream) -> io::Result<()> {
        self.running.store(false, Ordering::SeqCst);
        write_response(
            stream,
            200,
            &[
                ("Content-Type", "text/plain; charset=utf-8"),
                ("Content-Length", "13"),
                ("Connection", "close"),
            ],
            b"shutting down",
        )
    }

    fn remote_path(&self, request: &HttpRequest) -> PathRequest {
        let Some(path) = request_path(&request.target) else {
            return PathRequest::NotFound;
        };
        let Some(stripped) = strip_token_path(path, &self.token) else {
            return PathRequest::NotFound;
        };
        if stripped == SHUTDOWN_PATH {
            return PathRequest::Shutdown;
        }

        match percent_decode_path(stripped).and_then(|path| normalize_remote_path(&path).ok()) {
            Some(path) => PathRequest::Remote(path),
            None => PathRequest::NotFound,
        }
    }

    fn write_file_range(
        &mut self,
        stream: &mut TcpStream,
        handle: ObjectHandle,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        let mut cursor = offset;
        let end = offset.saturating_add(length);

        while cursor < end {
            let chunk_size = (end - cursor).min(PARTIAL_CHUNK_SIZE) as u32;
            match self.read_partial(handle, cursor, chunk_size) {
                Ok(bytes) => {
                    if bytes.is_empty() {
                        break;
                    }
                    stream.write_all(&bytes)?;
                    cursor += bytes.len() as u64;
                }
                Err(error) if is_partial_unsupported(&error) => {
                    log::debug!("partial MTP read unavailable; falling back to full-object stream");
                    return self.stream_file_range(stream, handle, cursor, end - cursor);
                }
                Err(error) => {
                    log::debug!("MTP range read failed: {error}");
                    return Err(io::Error::other(error.to_string()));
                }
            }
        }

        Ok(())
    }

    fn read_partial(
        &mut self,
        handle: ObjectHandle,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>, mtp_rs::Error> {
        if self.partial64_available {
            match self
                .runtime
                .block_on(self.storage.download_partial_64(handle, offset, size))
            {
                Ok(bytes) => return Ok(bytes),
                Err(error) if is_partial_unsupported(&error) => {
                    self.partial64_available = false;
                    if !self.partial32_available {
                        return Err(error);
                    }
                }
                Err(error) => return Err(error),
            }
        }

        if self.partial32_available && offset <= u32::MAX as u64 {
            return match self
                .runtime
                .block_on(self.storage.download_partial(handle, offset, size))
            {
                Ok(bytes) => Ok(bytes),
                Err(error) if is_partial_unsupported(&error) => {
                    self.partial32_available = false;
                    Err(error)
                }
                Err(error) => Err(error),
            };
        }

        Err(mtp_rs::Error::Protocol {
            code: ResponseCode::OperationNotSupported,
            operation: OperationCode::GetPartialObject,
        })
    }

    fn stream_file_range(
        &self,
        stream: &mut TcpStream,
        handle: ObjectHandle,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        let mut download = self
            .runtime
            .block_on(self.storage.download_stream(handle))
            .map_err(|error| io::Error::other(error.to_string()))?;
        let mut skipped = 0u64;
        let mut remaining = length;

        while remaining > 0 {
            let Some(chunk) = self.runtime.block_on(download.next_chunk()) else {
                break;
            };
            let bytes = chunk.map_err(|error| io::Error::other(error.to_string()))?;
            let mut slice = bytes.as_ref();

            if skipped < offset {
                let to_skip = (offset - skipped).min(slice.len() as u64) as usize;
                skipped += to_skip as u64;
                slice = &slice[to_skip..];
            }

            if slice.is_empty() {
                continue;
            }

            let to_write = remaining.min(slice.len() as u64) as usize;
            stream.write_all(&slice[..to_write])?;
            remaining -= to_write as u64;
        }

        if remaining == 0 {
            let _ = self
                .runtime
                .block_on(download.cancel(DEFAULT_CANCEL_TIMEOUT));
        }

        Ok(())
    }
}

enum PathRequest {
    Remote(String),
    Shutdown,
    NotFound,
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    start: u64,
    end: u64,
}

#[derive(Debug)]
struct DavEntry {
    href: String,
    display_name: String,
    is_folder: bool,
    size: u64,
    modified_http: String,
    created_iso: String,
    etag: String,
    content_type: &'static str,
}

impl DavEntry {
    fn root(token: &str) -> Self {
        Self {
            href: format!("/{token}/"),
            display_name: DEFAULT_VOLUME_NAME.to_string(),
            is_folder: true,
            size: 0,
            modified_http: fallback_http_date(),
            created_iso: fallback_creation_date(),
            etag: "\"tp7-root\"".to_string(),
            content_type: "httpd/unix-directory",
        }
    }

    fn from_object(token: &str, parent_path: &str, object: ObjectInfo) -> Self {
        let path = join_remote_path(parent_path, &object.filename);
        let is_folder = object.is_folder();
        let modified_http = object
            .modified
            .as_ref()
            .map(format_mtp_http_date)
            .unwrap_or_else(fallback_http_date);
        let created_iso = object
            .modified
            .as_ref()
            .map(format_mtp_creation_date)
            .unwrap_or_else(fallback_creation_date);
        let etag = format!("\"{}-{}-{}\"", object.handle.0, object.size, modified_http);
        let content_type = if is_folder {
            "httpd/unix-directory"
        } else {
            content_type_for_name(&object.filename)
        };

        Self {
            href: href_for_remote_path(token, &path, is_folder),
            display_name: object.filename,
            is_folder,
            size: object.size,
            modified_http,
            created_iso,
            etag,
            content_type,
        }
    }
}

#[derive(Debug)]
struct StoredMountState {
    state: MountState,
    path: PathBuf,
}

#[derive(Debug)]
struct UnmountTarget {
    mountpoint: String,
    state: Option<StoredMountState>,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> io::Result<Option<HttpRequest>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let request_line = request_line.trim_end_matches(['\r', '\n']);
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return Ok(None);
    };
    let Some(target) = parts.next() else {
        return Ok(None);
    };

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }

    if let Some(length) = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
    {
        let mut body = vec![0; length];
        reader.read_exact(&mut body)?;
    }

    Ok(Some(HttpRequest {
        method: method.to_string(),
        target: target.to_string(),
        headers,
    }))
}

fn write_server_error(stream: &mut TcpStream, error: &AppError) -> io::Result<()> {
    write_response(
        stream,
        500,
        &[
            ("Content-Type", "text/plain; charset=utf-8"),
            ("Content-Length", &error.to_string().len().to_string()),
            ("Connection", "close"),
        ],
        error.to_string().as_bytes(),
    )
}

fn write_empty_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
) -> io::Result<()> {
    write_response_head(stream, status, headers)?;
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> io::Result<()> {
    write_response_head(stream, status, headers)?;
    stream.write_all(body)
}

fn write_response_head(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, &str)],
) -> io::Result<()> {
    write!(stream, "HTTP/1.1 {} {}\r\n", status, reason_phrase(status))?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "\r\n")
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        207 => "Multi-Status",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn propfind_body(entries: &[DavEntry]) -> String {
    let mut body = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    body.push_str("<D:multistatus xmlns:D=\"DAV:\">\n");

    for entry in entries {
        body.push_str("  <D:response>\n");
        body.push_str("    <D:href>");
        body.push_str(&xml_escape(&entry.href));
        body.push_str("</D:href>\n");
        body.push_str("    <D:propstat>\n");
        body.push_str("      <D:prop>\n");
        body.push_str("        <D:displayname>");
        body.push_str(&xml_escape(&entry.display_name));
        body.push_str("</D:displayname>\n");
        body.push_str("        <D:creationdate>");
        body.push_str(&entry.created_iso);
        body.push_str("</D:creationdate>\n");
        body.push_str("        <D:getlastmodified>");
        body.push_str(&entry.modified_http);
        body.push_str("</D:getlastmodified>\n");
        body.push_str("        <D:getetag>");
        body.push_str(&xml_escape(&entry.etag));
        body.push_str("</D:getetag>\n");
        body.push_str("        <D:resourcetype>");
        if entry.is_folder {
            body.push_str("<D:collection/>");
        }
        body.push_str("</D:resourcetype>\n");
        body.push_str("        <D:supportedlock/>\n");
        body.push_str("        <D:getcontenttype>");
        body.push_str(entry.content_type);
        body.push_str("</D:getcontenttype>\n");
        if !entry.is_folder {
            body.push_str("        <D:getcontentlength>");
            body.push_str(&entry.size.to_string());
            body.push_str("</D:getcontentlength>\n");
        }
        body.push_str("      </D:prop>\n");
        body.push_str("      <D:status>HTTP/1.1 200 OK</D:status>\n");
        body.push_str("    </D:propstat>\n");
        body.push_str("  </D:response>\n");
    }

    body.push_str("</D:multistatus>\n");
    body
}

fn parse_range_header(header: &str, size: u64) -> Result<ByteRange, ()> {
    let Some(range) = header.strip_prefix("bytes=") else {
        return Err(());
    };
    if range.contains(',') || size == 0 {
        return Err(());
    }
    let Some((start, end)) = range.split_once('-') else {
        return Err(());
    };

    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let start = size.saturating_sub(suffix);
        return Ok(ByteRange {
            start,
            end: size - 1,
        });
    }

    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(size - 1)
    };
    if end < start {
        return Err(());
    }

    Ok(ByteRange { start, end })
}

fn request_path(target: &str) -> Option<&str> {
    let target = target.split_once('?').map_or(target, |(path, _)| path);
    if target.starts_with('/') {
        return Some(target);
    }
    if let Some(after_scheme) = target.split_once("://").map(|(_, rest)| rest) {
        return after_scheme
            .find('/')
            .map(|index| &after_scheme[index..])
            .or(Some("/"));
    }
    None
}

fn strip_token_path<'a>(path: &'a str, token: &str) -> Option<&'a str> {
    let prefix = format!("/{token}");
    if path == prefix {
        return Some("/");
    }
    path.strip_prefix(&(prefix + "/"))
        .map(|stripped| if stripped.is_empty() { "/" } else { stripped })
}

fn percent_decode_path(path: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hi = hex_value(bytes[index + 1])?;
                let lo = hex_value(bytes[index + 2])?;
                decoded.push((hi << 4) | lo);
                index += 3;
            }
            b'%' => return None,
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn href_for_remote_path(token: &str, remote_path: &str, is_folder: bool) -> String {
    let mut href = format!("/{token}");
    if remote_path != "/" {
        for component in remote_path.trim_matches('/').split('/') {
            href.push('/');
            href.push_str(&percent_encode_component(component));
        }
    }
    if is_folder {
        href.push('/');
    }
    href
}

fn percent_encode_component(component: &str) -> String {
    let mut encoded = String::new();
    for byte in component.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(*byte as char);
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn parent_path(path: &str) -> &str {
    path.rsplit_once('/').map_or(
        "/",
        |(parent, _)| {
            if parent.is_empty() { "/" } else { parent }
        },
    )
}

fn content_type_for_name(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase());
    match extension.as_deref() {
        Some("wav") | Some("wave") => "audio/wav",
        Some("aif") | Some("aiff") => "audio/aiff",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
}

fn format_mtp_http_date(datetime: &mtp_rs::ptp::DateTime) -> String {
    let weekday = weekday_name(datetime.year as i32, datetime.month, datetime.day);
    let month = month_name(datetime.month);
    format!(
        "{weekday}, {:02} {month} {:04} {:02}:{:02}:{:02} GMT",
        datetime.day, datetime.year, datetime.hour, datetime.minute, datetime.second
    )
}

fn format_mtp_creation_date(datetime: &mtp_rs::ptp::DateTime) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        datetime.year,
        datetime.month,
        datetime.day,
        datetime.hour,
        datetime.minute,
        datetime.second
    )
}

fn fallback_http_date() -> String {
    "Thu, 01 Jan 1970 00:00:00 GMT".to_string()
}

fn fallback_creation_date() -> String {
    "1970-01-01T00:00:00Z".to_string()
}

fn weekday_name(year: i32, month: u8, day: u8) -> &'static str {
    let mut y = year;
    let mut m = month as i32;
    if m < 3 {
        m += 12;
        y -= 1;
    }
    let k = y % 100;
    let j = y / 100;
    let h = (day as i32 + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 + 5 * j).rem_euclid(7);
    match h {
        0 => "Sat",
        1 => "Sun",
        2 => "Mon",
        3 => "Tue",
        4 => "Wed",
        5 => "Thu",
        _ => "Fri",
    }
}

fn month_name(month: u8) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "Jan",
    }
}

fn is_partial_unsupported(error: &mtp_rs::Error) -> bool {
    matches!(
        error,
        mtp_rs::Error::Protocol {
            code: ResponseCode::OperationNotSupported
                | ResponseCode::ParameterNotSupported
                | ResponseCode::InvalidObjectHandle,
            ..
        }
    )
}

fn prepare_mountpoint(mountpoint: Option<&str>) -> Result<PathBuf, AppError> {
    let path = match mountpoint {
        Some(path) => PathBuf::from(path),
        None => default_mountpoint()?,
    };
    let path = absolute_path(path)?;

    if path.exists() {
        if !path.is_dir() {
            return Err(AppError::LocalPathIsFile {
                path: path_to_string(&path),
            });
        }
        if !is_mounted(&path) && !is_directory_empty(&path)? {
            return Err(AppError::FileSystem {
                path: path_to_string(&path),
                message: "mount point must be empty".to_string(),
            });
        }
    } else {
        fs::create_dir_all(&path).map_err(|error| AppError::FileSystem {
            path: path_to_string(&path),
            message: error.to_string(),
        })?;
    }

    Ok(path)
}

fn default_mountpoint() -> Result<PathBuf, AppError> {
    for index in 0..100 {
        let name = if index == 0 {
            DEFAULT_VOLUME_NAME.to_string()
        } else {
            format!("{DEFAULT_VOLUME_NAME} {}", index + 1)
        };
        let path = PathBuf::from(DEFAULT_MOUNT_ROOT).join(name);
        if !path.exists() {
            return Ok(path);
        }
        if path.is_dir() && !is_mounted(&path) && is_directory_empty(&path)? {
            return Ok(path);
        }
    }

    Err(AppError::Mount {
        message: "could not find an available /Volumes/TP-7 mount point".to_string(),
    })
}

fn absolute_path(path: PathBuf) -> Result<PathBuf, AppError> {
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

fn is_directory_empty(path: &Path) -> Result<bool, AppError> {
    let mut entries = fs::read_dir(path).map_err(|error| AppError::FileSystem {
        path: path_to_string(path),
        message: error.to_string(),
    })?;
    Ok(entries.next().is_none())
}

fn mount_webdav(url: &str, mountpoint: &Path) -> Result<(), AppError> {
    let output = Command::new("/sbin/mount_webdav")
        .arg("-S")
        .arg("-v")
        .arg(DEFAULT_VOLUME_NAME)
        .arg("-o")
        .arg("rdonly,nodev,nosuid,noexec")
        .arg(url)
        .arg(mountpoint)
        .output()
        .map_err(|error| AppError::Mount {
            message: format!("could not run mount_webdav: {error}"),
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(AppError::Mount {
        message: if message.is_empty() {
            format!("mount_webdav exited with {}", output.status)
        } else {
            message
        },
    })
}

fn is_mounted(path: &Path) -> bool {
    let Ok(output) = Command::new("/sbin/mount").output() else {
        return false;
    };
    let mountpoint = path_to_string(path);
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.contains(&format!(" on {mountpoint} (")))
}

fn unmount_target(mountpoint: &str, state: Option<&StoredMountState>) -> Result<(), AppError> {
    let path = PathBuf::from(mountpoint);
    let mut unmount_error = None;
    let mounted = is_mounted(&path);

    if mounted {
        if let Err(error) = diskutil_unmount(&path).or_else(|_| umount_path(&path)) {
            unmount_error = Some(error);
        }
    } else if state.is_none() {
        return Err(AppError::Unmount {
            message: "mount point is not mounted and has no TP-7 mount record".to_string(),
        });
    }

    if let Some(state) = state {
        if let Err(error) = send_shutdown(&state.state) {
            log::debug!("mount server shutdown request failed: {error}");
            terminate_pid(state.state.pid);
        }
    }

    if let Some(error) = unmount_error {
        return Err(error);
    }

    Ok(())
}

fn diskutil_unmount(path: &Path) -> Result<(), AppError> {
    let output = Command::new("/usr/sbin/diskutil")
        .arg("unmount")
        .arg(path)
        .output()
        .map_err(|error| AppError::Unmount {
            message: format!("could not run diskutil: {error}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(AppError::Unmount {
        message: command_error_message("diskutil unmount", &output),
    })
}

fn umount_path(path: &Path) -> Result<(), AppError> {
    let output = Command::new("/sbin/umount")
        .arg(path)
        .output()
        .map_err(|error| AppError::Unmount {
            message: format!("could not run umount: {error}"),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(AppError::Unmount {
        message: command_error_message("umount", &output),
    })
}

fn command_error_message(command: &str, output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stderr.is_empty() {
        format!("{command}: {stderr}")
    } else if !stdout.is_empty() {
        format!("{command}: {stdout}")
    } else {
        format!("{command} exited with {}", output.status)
    }
}

fn send_shutdown(state: &MountState) -> Result<(), AppError> {
    let address = format!("127.0.0.1:{}", state.port);
    let mut stream = TcpStream::connect(address).map_err(|error| AppError::Unmount {
        message: format!("could not contact mount server: {error}"),
    })?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| AppError::Unmount {
            message: format!("could not configure shutdown request: {error}"),
        })?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|error| AppError::Unmount {
            message: format!("could not configure shutdown request: {error}"),
        })?;
    let request = format!(
        "GET /{}{SHUTDOWN_PATH} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        state.token
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|error| AppError::Unmount {
            message: format!("could not send shutdown request: {error}"),
        })?;
    Ok(())
}

fn terminate_pid(pid: u32) {
    let _ = Command::new("/bin/kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
}

fn unmount_targets(
    mountpoint: Option<&str>,
    states: Vec<StoredMountState>,
) -> Result<Vec<UnmountTarget>, AppError> {
    let Some(mountpoint) = mountpoint else {
        return Ok(states
            .into_iter()
            .map(|state| UnmountTarget {
                mountpoint: state.state.mountpoint.clone(),
                state: Some(state),
            })
            .collect());
    };

    let requested = path_to_string(&absolute_path(PathBuf::from(mountpoint))?);
    let mut targets = Vec::new();
    let mut matched = false;
    for state in states {
        if state.state.mountpoint == requested {
            matched = true;
            targets.push(UnmountTarget {
                mountpoint: requested.clone(),
                state: Some(state),
            });
        }
    }

    if !matched {
        targets.push(UnmountTarget {
            mountpoint: requested,
            state: None,
        });
    }

    Ok(targets)
}

fn state_dir() -> Result<PathBuf, AppError> {
    let home = std::env::var_os("HOME").ok_or_else(|| AppError::FileSystem {
        path: "$HOME".to_string(),
        message: "HOME is not set".to_string(),
    })?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("tp7")
        .join("mounts"))
}

fn write_mount_state(state: &MountState) -> Result<PathBuf, AppError> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir).map_err(|error| AppError::FileSystem {
        path: path_to_string(&dir),
        message: error.to_string(),
    })?;
    let path = dir.join(format!("mount-{}.json", state.pid));
    let bytes = serde_json::to_vec_pretty(state).map_err(|source| AppError::Json { source })?;
    fs::write(&path, bytes).map_err(|error| AppError::FileSystem {
        path: path_to_string(&path),
        message: error.to_string(),
    })?;
    Ok(path)
}

fn read_mount_states() -> Result<Vec<StoredMountState>, AppError> {
    let dir = state_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut states = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|error| AppError::FileSystem {
        path: path_to_string(&dir),
        message: error.to_string(),
    })? {
        let entry = entry.map_err(|error| AppError::FileSystem {
            path: path_to_string(&dir),
            message: error.to_string(),
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let bytes = fs::read(&path).map_err(|error| AppError::FileSystem {
            path: path_to_string(&path),
            message: error.to_string(),
        })?;
        match serde_json::from_slice::<MountState>(&bytes) {
            Ok(state) => states.push(StoredMountState { state, path }),
            Err(error) => log::debug!("ignoring invalid mount state {}: {error}", path.display()),
        }
    }

    Ok(states)
}

fn remove_state_file(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        log::debug!("failed to remove mount state {}: {error}", path.display());
    }
}

fn mount_token() -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::process::id().hash(&mut hasher);
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .hash(&mut hasher);
    format!("tp7-{:016x}", hasher.finish())
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use mtp_rs::ptp::DateTime;

    use super::*;

    #[test]
    fn parses_regular_byte_range() {
        assert_eq!(
            parse_range_header("bytes=10-19", 100).unwrap(),
            ByteRange { start: 10, end: 19 }
        );
    }

    #[test]
    fn parses_open_ended_byte_range() {
        assert_eq!(
            parse_range_header("bytes=95-", 100).unwrap(),
            ByteRange { start: 95, end: 99 }
        );
    }

    #[test]
    fn parses_suffix_byte_range() {
        assert_eq!(
            parse_range_header("bytes=-5", 100).unwrap(),
            ByteRange { start: 95, end: 99 }
        );
    }

    #[test]
    fn rejects_out_of_bounds_byte_range() {
        assert!(parse_range_header("bytes=100-101", 100).is_err());
    }

    #[test]
    fn strips_token_prefix() {
        assert_eq!(strip_token_path("/tp7-token", "tp7-token"), Some("/"));
        assert_eq!(
            strip_token_path("/tp7-token/memo/a.wav", "tp7-token"),
            Some("memo/a.wav")
        );
        assert_eq!(strip_token_path("/other/memo", "tp7-token"), None);
    }

    #[test]
    fn decodes_percent_encoded_paths() {
        assert_eq!(
            percent_decode_path("/memo/take%201.wav").unwrap(),
            "/memo/take 1.wav"
        );
    }

    #[test]
    fn encodes_href_components() {
        assert_eq!(
            href_for_remote_path("tp7-token", "/memo/take 1.wav", false),
            "/tp7-token/memo/take%201.wav"
        );
    }

    #[test]
    fn formats_mtp_http_date() {
        let datetime = DateTime::new(2026, 5, 8, 12, 30, 45).unwrap();
        assert_eq!(
            format_mtp_http_date(&datetime),
            "Fri, 08 May 2026 12:30:45 GMT"
        );
    }
}
