//! Peer-UID-authenticated local transport for the macOS system service.
//!
//! The macOS daemon must read its owner UID from protected installation state
//! and keep its socket in the root-owned application-support directory. The
//! socket is owned by that user with mode 0600; requests are also checked
//! against the UID supplied by the kernel.

use std::{io, sync::Arc, time::Duration};

use focuser_app::{
    AppContext, Command, CommandErrorPayload,
    command::WebsiteRuleKind,
    protocol::{self, RequestEnvelope, ResponseEnvelope},
};
use focuser_common::types::WebsiteMatchType;
use tokio::{
    io::AsyncWriteExt,
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
    time::timeout,
};

const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIENTS: usize = 32;

pub async fn serve(
    listener: UnixListener,
    owner_uid: u32,
    context: Arc<AppContext>,
    policy_enabled: bool,
) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut tasks = JoinSet::new();

    loop {
        let (stream, _) = listener.accept().await?;
        let peer_uid = match stream.peer_cred() {
            Ok(credentials) => credentials.uid(),
            Err(error) => {
                tracing::debug!(%error, "could not identify Unix-socket peer");
                continue;
            }
        };
        if peer_uid != owner_uid {
            tracing::warn!(peer_uid, "rejected Unix-socket peer with unexpected UID");
            continue;
        }

        let permit = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("Unix-socket server is shutting down"))?;
        let client_context = Arc::clone(&context);
        tasks.spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_connection(stream, &client_context, policy_enabled).await {
                tracing::debug!(%error, "rejected or failed Unix-socket request");
            }
        });
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(%error, "Unix-socket request task panicked");
            }
        }
    }
}

async fn handle_connection(
    mut stream: UnixStream,
    context: &AppContext,
    policy_enabled: bool,
) -> io::Result<()> {
    let request: RequestEnvelope =
        timeout(CLIENT_TIMEOUT, protocol::read_request_async(&mut stream))
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "request frame deadline elapsed")
            })??;

    let response = if matches!(&request.command, Command::AppVersion)
        || (policy_enabled && macos_service_command_allowed(&request.command))
    {
        protocol::dispatch_service_request(context, request)
    } else {
        ResponseEnvelope::new(
            request.request_id,
            Err(CommandErrorPayload {
                code: if policy_enabled {
                    "service_command_not_supported"
                } else {
                    "service_smoke_only"
                }
                .into(),
                message: if policy_enabled {
                    "The macOS service currently supports domain-based website policy only.".into()
                } else {
                    "The service smoke host supports only its version probe.".into()
                },
            }),
        )
    };

    timeout(
        CLIENT_TIMEOUT,
        protocol::write_response_async(&mut stream, &response),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response write deadline elapsed"))??;
    stream.flush().await
}

fn macos_service_command_allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::ListBlockLists
            | Command::CreateBlockList { .. }
            | Command::DeleteBlockList { .. }
            | Command::ToggleBlockList { .. }
            | Command::AddWebsiteRule {
                rule: WebsiteMatchType::Domain(_),
                ..
            }
            | Command::RemoveWebsiteRule { .. }
            | Command::BulkImportWebsites {
                kind: WebsiteRuleKind::Domain,
                ..
            }
            | Command::ClearAllWebsites
            | Command::UpdateSchedule { .. }
            | Command::GetBlockingHealth
            | Command::GetExtensionRules
            | Command::ExtensionHeartbeat { .. }
            | Command::RecordExtensionAllowanceTick { .. }
            | Command::RecordExtensionSharedActivityTick { .. }
            | Command::ApplyBlocks
    )
}

#[cfg(target_os = "macos")]
pub async fn bind_macos_listener(owner_uid: u32) -> io::Result<(UnixListener, SocketPathGuard)> {
    use focuser_common::ipc::FOCUSGUARD_SERVICE_SOCKET_PATH;

    bind_secure_socket(
        std::path::Path::new(FOCUSGUARD_SERVICE_SOCKET_PATH),
        owner_uid,
        true,
    )
    .await
}

#[cfg(unix)]
async fn bind_secure_socket(
    path: &std::path::Path,
    owner_uid: u32,
    root_service: bool,
) -> io::Result<(UnixListener, SocketPathGuard)> {
    use std::{
        ffi::CString,
        fs,
        os::unix::{
            ffi::OsStrExt,
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
        },
    };

    let effective_uid = unsafe { libc::geteuid() };
    if root_service && effective_uid != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the system service must run as root",
        ));
    }
    if owner_uid == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the configured service owner must not be root",
        ));
    }

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "service socket has no parent directory",
        )
    })?;
    let directory = fs::symlink_metadata(parent)?;
    let expected_directory_uid = if root_service { 0 } else { effective_uid };
    if !directory.file_type().is_dir()
        || directory.uid() != expected_directory_uid
        || directory.mode() & 0o022 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "service socket parent must be a protected directory owned by its service account",
        ));
    }

    remove_stale_socket(path).await?;
    let listener = UnixListener::bind(path)?;
    let socket_c = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "service socket path contains NUL",
        )
    })?;
    let setup = (|| {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        let status = unsafe {
            libc::chown(
                socket_c.as_ptr(),
                owner_uid as libc::uid_t,
                !0 as libc::gid_t,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != owner_uid
            || metadata.mode() & 0o777 != 0o600
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "service socket ownership or permissions did not match policy",
            ));
        }
        Ok(())
    })();
    if let Err(error) = setup {
        let _ = fs::remove_file(path);
        return Err(error);
    }

    Ok((
        listener,
        SocketPathGuard {
            path: path.to_path_buf(),
            owner_uid,
        },
    ))
}

#[cfg(unix)]
async fn remove_stale_socket(path: &std::path::Path) -> io::Result<()> {
    use std::{fs, os::unix::fs::FileTypeExt};

    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to remove an unexpected service socket path",
        ));
    }
    match UnixStream::connect(path).await {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "another FocusGuard service is already listening",
        )),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
pub struct SocketPathGuard {
    path: std::path::PathBuf,
    owner_uid: u32,
}

#[cfg(unix)]
impl Drop for SocketPathGuard {
    fn drop(&mut self) {
        use std::{
            fs,
            os::unix::fs::{FileTypeExt, MetadataExt},
        };

        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.uid() == self.owner_uid
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{sync::Arc, time::Duration};

    use focuser_app::{
        AppContext, Command,
        protocol::{self, RequestEnvelope},
    };
    use focuser_core::{BlockEngine, Database};
    use tempfile::tempdir;
    use tokio::{
        net::{UnixListener, UnixStream},
        time::timeout,
    };
    use uuid::Uuid;

    use super::{bind_secure_socket, serve};

    fn context() -> Arc<AppContext> {
        let database = Database::open_in_memory().unwrap();
        let engine = BlockEngine::new(database).unwrap();
        Arc::new(AppContext::new_headless(engine))
    }

    async fn request(path: &std::path::Path) -> std::io::Result<protocol::ResponseEnvelope> {
        let mut stream = UnixStream::connect(path).await?;
        let request = RequestEnvelope::new(Uuid::new_v4(), Command::AppVersion);
        protocol::write_request_async(&mut stream, &request).await?;
        protocol::read_response_async(&mut stream).await
    }

    #[tokio::test]
    async fn unix_server_answers_the_version_probe_in_smoke_mode() {
        let directory = tempdir().unwrap();
        let socket_path = directory.path().join("service.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let owner_uid = unsafe { libc::geteuid() };
        let server = tokio::spawn(serve(listener, owner_uid, context(), false));

        let response = timeout(Duration::from_secs(2), request(&socket_path))
            .await
            .unwrap()
            .unwrap();
        assert!(response.result.is_ok());

        server.abort();
    }

    #[test]
    fn macos_service_allowlist_excludes_application_rules() {
        assert!(super::macos_service_command_allowed(
            &Command::GetBlockingHealth
        ));
        assert!(!super::macos_service_command_allowed(
            &Command::ClearAllApps
        ));
    }

    #[tokio::test]
    async fn secure_socket_is_owner_only_and_removes_its_path_on_drop() {
        use std::os::unix::fs::MetadataExt;

        let directory = tempdir().unwrap();
        let socket_path = directory.path().join("service.sock");
        let process_uid = unsafe { libc::geteuid() };
        let owner_uid = if process_uid == 0 { 1 } else { process_uid };
        let (listener, guard) = bind_secure_socket(&socket_path, owner_uid, false)
            .await
            .unwrap();
        drop(listener);

        let metadata = std::fs::symlink_metadata(&socket_path).unwrap();
        assert_eq!(metadata.uid(), owner_uid);
        assert_eq!(metadata.mode() & 0o777, 0o600);

        drop(guard);
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn secure_socket_refuses_an_already_listening_endpoint() {
        let directory = tempdir().unwrap();
        let socket_path = directory.path().join("service.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let process_uid = unsafe { libc::geteuid() };
        let owner_uid = if process_uid == 0 { 1 } else { process_uid };

        let error = match bind_secure_socket(&socket_path, owner_uid, false).await {
            Ok((listener, guard)) => {
                drop(listener);
                drop(guard);
                panic!("an active service socket should not be replaced");
            }
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
        drop(listener);
    }

    #[tokio::test]
    async fn unix_server_drops_connections_from_an_unconfigured_uid() {
        let directory = tempdir().unwrap();
        let socket_path = directory.path().join("service.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let current_uid = unsafe { libc::geteuid() };
        let other_uid = if current_uid == 0 { 1 } else { 0 };
        let server = tokio::spawn(serve(listener, other_uid, context(), true));

        let result = timeout(Duration::from_secs(2), request(&socket_path))
            .await
            .expect("unauthorized peer must be closed promptly");
        assert!(result.is_err());

        server.abort();
    }
}
