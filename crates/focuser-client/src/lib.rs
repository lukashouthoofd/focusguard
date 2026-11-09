//! Authenticated clients for the FocusGuard Windows and macOS policy services.
//!
//! The production client compares the named-pipe server PID with the PID SCM
//! reports for the running FocusGuard service before it sends a command. This
//! lets an unelevated client identify a SYSTEM service without opening its
//! process token. Isolated tests can instead pin a known process ID.

use std::io;
#[cfg(any(windows, target_os = "macos", all(test, unix)))]
use std::time::Duration;

use focuser_app::{Command, CommandErrorPayload, CommandResult};
use thiserror::Error;
#[cfg(any(windows, target_os = "macos", all(test, unix)))]
use uuid::Uuid;

pub use focuser_common::ipc::FOCUSGUARD_SERVICE_PIPE_NAME as SERVICE_PIPE_NAME;

#[cfg(any(windows, target_os = "macos", all(test, unix)))]
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(windows, target_os = "macos", all(test, unix)))]
const CONNECT_RETRY_WINDOW: Duration = Duration::from_secs(2);
#[cfg(any(windows, target_os = "macos", all(test, unix)))]
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(25);
#[cfg(windows)]
const SERVICE_NAME: &str = "FocusGuardService";
#[cfg(target_os = "macos")]
const MACOS_DAEMON_PLIST: &str = "com.publikhq.focusguard.daemon.plist";

#[cfg(windows)]
enum ServerIdentity<'a> {
    ScmService(&'a str),
    ProcessId(u32),
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("FocusGuard service is unavailable: {0}")]
    Connect(#[source] io::Error),

    #[error("could not query FocusGuard service registration in SCM: {0}")]
    ScmRegistration(#[source] io::Error),

    #[error("could not authenticate the FocusGuard service endpoint: {0}")]
    ServerIdentity(#[source] io::Error),

    #[error("could not query FocusGuard service registration: {0}")]
    ServiceRegistration(#[source] io::Error),

    #[error("FocusGuard service did not respond within five seconds")]
    Timeout,

    #[error("invalid FocusGuard service response: {0}")]
    InvalidResponse(&'static str),

    #[error("FocusGuard service protocol error: {0}")]
    Protocol(#[source] io::Error),

    #[error("FocusGuard service rejected the command ({0:?})")]
    Service(CommandErrorPayload),

    #[error("FocusGuard service client is only available on Windows and macOS")]
    UnsupportedPlatform,
}

/// Whether this installation must use service mode, including a registered
/// service that is stopped or a macOS install whose privileged policy marker
/// remains after launchd unregistration. The desktop uses this to avoid opening
/// a stale per-user database as a second policy writer.
pub fn is_service_registered() -> Result<bool, ClientError> {
    #[cfg(windows)]
    {
        service_registered_windows().map_err(ClientError::ScmRegistration)
    }
    #[cfg(target_os = "macos")]
    {
        use smappservice_rs::ServiceStatus;

        let status = macos_service().status();
        match status {
            ServiceStatus::Enabled | ServiceStatus::RequiresApproval => Ok(true),
            // A marker means this installation has already moved policy into
            // the root-owned database. If launchd is currently unregistered,
            // stay in service-client mode so edits cannot silently go to the
            // old per-user database and disappear on a later re-registration.
            ServiceStatus::NotRegistered => focuser_common::ipc::read_macos_service_owner_uid()
                .map(|owner_uid| owner_uid.is_some())
                .map_err(ClientError::ServiceRegistration),
            // A missing bundle plist after prior registration is an uncertain
            // installation state. Keep the local policy writer disabled when
            // the root-owned marker proves this host had the service before.
            ServiceStatus::NotFound => focuser_common::ipc::read_macos_service_owner_uid()
                .map(|owner_uid| owner_uid.is_some())
                .map_err(ClientError::ServiceRegistration),
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Ok(false)
    }
}

/// Return the macOS LaunchDaemon authorization state for the Settings screen.
///
/// SMAppService reports registration separately from approval. In particular,
/// `requires_approval` means the user still needs to enable FocusGuard under
/// System Settings > General > Login Items.
pub fn macos_service_status() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        use smappservice_rs::ServiceStatus;

        match macos_service().status() {
            ServiceStatus::Enabled => "enabled",
            ServiceStatus::RequiresApproval => "requires_approval",
            ServiceStatus::NotRegistered => "not_registered",
            ServiceStatus::NotFound => "not_found",
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        "unsupported"
    }
}

/// Register the bundled macOS system daemon. The OS may still require the
/// user to approve it in Login Items before launchd starts it.
pub fn register_macos_service() -> Result<(), ClientError> {
    #[cfg(target_os = "macos")]
    {
        use smappservice_rs::ServiceStatus;

        let service = macos_service();
        match service.status() {
            ServiceStatus::Enabled | ServiceStatus::RequiresApproval => Ok(()),
            ServiceStatus::NotRegistered | ServiceStatus::NotFound => {
                service.register().map_err(|error| {
                    ClientError::ServiceRegistration(io::Error::other(error.to_string()))
                })
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ClientError::UnsupportedPlatform)
    }
}

/// Open the user-controlled Login Items pane so they can approve or disable
/// the LaunchDaemon.
pub fn open_macos_service_settings() -> Result<(), ClientError> {
    #[cfg(target_os = "macos")]
    {
        smappservice_rs::AppService::open_system_settings_login_items();
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(ClientError::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
fn macos_service() -> smappservice_rs::AppService {
    smappservice_rs::AppService::new(smappservice_rs::ServiceType::Daemon {
        plist_name: MACOS_DAEMON_PLIST,
    })
}

/// Execute a command through the installed Windows service.
///
/// SCM must report the pipe server PID as the running, own-process
/// `FocusGuardService`. The command is sent only after that identity check
/// succeeds.
pub async fn execute(command: Command) -> Result<CommandResult, ClientError> {
    #[cfg(windows)]
    {
        execute_windows(
            SERVICE_PIPE_NAME,
            command,
            ServerIdentity::ScmService(SERVICE_NAME),
        )
        .await
    }
    #[cfg(target_os = "macos")]
    {
        execute_unix(
            std::path::Path::new(focuser_common::ipc::FOCUSGUARD_SERVICE_SOCKET_PATH),
            0,
            command,
        )
        .await
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = command;
        Err(ClientError::UnsupportedPlatform)
    }
}

#[cfg(windows)]
fn service_registered_windows() -> io::Result<bool> {
    use std::{os::windows::ffi::OsStrExt, ptr::null_mut};

    use windows_sys::Win32::{
        Foundation::ERROR_SERVICE_DOES_NOT_EXIST,
        System::Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT,
            SERVICE_QUERY_STATUS,
        },
    };

    struct ServiceHandle(*mut core::ffi::c_void);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe { CloseServiceHandle(self.0) };
        }
    }

    let manager = unsafe { OpenSCManagerW(null_mut(), null_mut(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!("OpenSCManagerW(SC_MANAGER_CONNECT): {error}"),
        ));
    }
    let manager = ServiceHandle(manager);

    let service_name: Vec<u16> = std::ffi::OsStr::new(SERVICE_NAME)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let service = unsafe { OpenServiceW(manager.0, service_name.as_ptr(), SERVICE_QUERY_STATUS) };
    if !service.is_null() {
        let service = ServiceHandle(service);
        drop(service);
        return Ok(true);
    }

    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
        Ok(false)
    } else {
        Err(io::Error::new(
            error.kind(),
            format!("OpenServiceW(FocusGuardService, SERVICE_QUERY_STATUS): {error}"),
        ))
    }
}

/// Execute against a named pipe whose server process ID is known by the caller.
///
/// This supports isolated service instances and integration tests. Production
/// desktop code should use [`execute`], which checks the server PID through SCM.
pub async fn execute_for_process(
    pipe_name: &str,
    expected_server_pid: u32,
    command: Command,
) -> Result<CommandResult, ClientError> {
    #[cfg(windows)]
    {
        execute_windows(
            pipe_name,
            command,
            ServerIdentity::ProcessId(expected_server_pid),
        )
        .await
    }
    #[cfg(not(windows))]
    {
        let _ = (pipe_name, expected_server_pid, command);
        Err(ClientError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "macos", all(test, unix)))]
async fn execute_unix(
    socket_path: &std::path::Path,
    expected_server_uid: u32,
    command: Command,
) -> Result<CommandResult, ClientError> {
    use tokio::{
        io::AsyncWriteExt,
        net::UnixStream,
        time::{Instant, sleep, timeout},
    };

    let deadline = Instant::now() + CONNECT_RETRY_WINDOW;
    let mut stream = loop {
        match UnixStream::connect(socket_path).await {
            Ok(stream) => break stream,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                ) && Instant::now() < deadline =>
            {
                sleep(CONNECT_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(ClientError::Connect(error)),
        }
    };

    let peer_uid = stream
        .peer_cred()
        .map_err(ClientError::ServerIdentity)?
        .uid();
    if peer_uid != expected_server_uid {
        return Err(ClientError::ServerIdentity(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Unix-socket peer UID is not the expected service identity",
        )));
    }

    let request_id = Uuid::new_v4();
    let request = focuser_app::protocol::RequestEnvelope::new(request_id, command);
    let response = timeout(RESPONSE_TIMEOUT, async {
        focuser_app::protocol::write_request_async(&mut stream, &request)
            .await
            .map_err(ClientError::Protocol)?;
        stream.flush().await.map_err(ClientError::Protocol)?;
        focuser_app::protocol::read_response_async(&mut stream)
            .await
            .map_err(ClientError::Protocol)
    })
    .await
    .map_err(|_| ClientError::Timeout)??;

    if response.request_id != request_id {
        return Err(ClientError::InvalidResponse(
            "request identifier does not match",
        ));
    }
    response.result.map_err(ClientError::Service)
}

#[cfg(windows)]
async fn execute_windows(
    pipe_name: &str,
    command: Command,
    expected_server: ServerIdentity<'_>,
) -> Result<CommandResult, ClientError> {
    use tokio::{
        io::AsyncWriteExt,
        net::windows::named_pipe::ClientOptions,
        time::{Instant, sleep, timeout},
    };

    let deadline = Instant::now() + CONNECT_RETRY_WINDOW;
    let mut pipe = loop {
        match ClientOptions::new().open(pipe_name) {
            Ok(pipe) => break pipe,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::NotFound
                ) && Instant::now() < deadline =>
            {
                sleep(CONNECT_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(ClientError::Connect(error)),
        }
    };

    let identity_check = match expected_server {
        ServerIdentity::ScmService(service_name) => verify_scm_service_process(&pipe, service_name),
        ServerIdentity::ProcessId(expected_pid) => verify_server_pid(&pipe, expected_pid),
    };
    identity_check.map_err(ClientError::ServerIdentity)?;

    let request_id = Uuid::new_v4();
    let request = focuser_app::protocol::RequestEnvelope::new(request_id, command);
    let response = timeout(RESPONSE_TIMEOUT, async {
        focuser_app::protocol::write_request_async(&mut pipe, &request)
            .await
            .map_err(ClientError::Protocol)?;
        pipe.flush().await.map_err(ClientError::Protocol)?;
        focuser_app::protocol::read_response_async(&mut pipe)
            .await
            .map_err(ClientError::Protocol)
    })
    .await
    .map_err(|_| ClientError::Timeout)??;

    if response.request_id != request_id {
        return Err(ClientError::InvalidResponse(
            "request identifier does not match",
        ));
    }

    response.result.map_err(ClientError::Service)
}

#[cfg(windows)]
fn named_pipe_server_pid(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
) -> io::Result<u32> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{Foundation::HANDLE, System::Pipes::GetNamedPipeServerProcessId};

    let mut server_pid = 0u32;
    if unsafe { GetNamedPipeServerProcessId(pipe.as_raw_handle() as HANDLE, &mut server_pid) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(server_pid)
}

#[cfg(windows)]
fn verify_scm_service_process(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
    service_name: &str,
) -> io::Result<()> {
    use std::{mem::size_of, os::windows::ffi::OsStrExt, ptr::null_mut};

    use windows_sys::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_MANAGER_CONNECT,
        SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
        SERVICE_WIN32_OWN_PROCESS,
    };

    struct ServiceHandle(*mut core::ffi::c_void);
    impl Drop for ServiceHandle {
        fn drop(&mut self) {
            unsafe { CloseServiceHandle(self.0) };
        }
    }

    let manager = unsafe { OpenSCManagerW(null_mut(), null_mut(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(io::Error::last_os_error());
    }
    let manager = ServiceHandle(manager);

    let service_name_wide: Vec<u16> = std::ffi::OsStr::new(service_name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let service =
        unsafe { OpenServiceW(manager.0, service_name_wide.as_ptr(), SERVICE_QUERY_STATUS) };
    if service.is_null() {
        return Err(io::Error::last_os_error());
    }
    let service = ServiceHandle(service);

    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut returned_bytes = 0u32;
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&mut status as *mut SERVICE_STATUS_PROCESS).cast(),
            size_of::<SERVICE_STATUS_PROCESS>() as u32,
            &mut returned_bytes,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if returned_bytes as usize != size_of::<SERVICE_STATUS_PROCESS>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SCM returned an invalid service process status",
        ));
    }

    let server_pid = named_pipe_server_pid(pipe)?;
    if status.dwCurrentState != SERVICE_RUNNING
        || status.dwProcessId == 0
        || status.dwServiceType & SERVICE_WIN32_OWN_PROCESS == 0
        || status.dwProcessId != server_pid
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "named-pipe server is not the running FocusGuard service process",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_server_pid(
    pipe: &tokio::net::windows::named_pipe::NamedPipeClient,
    expected_pid: u32,
) -> io::Result<()> {
    let server_pid = named_pipe_server_pid(pipe)?;
    if server_pid != expected_pid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "named-pipe server process ID does not match the expected process",
        ));
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod unix_tests {
    use focuser_app::{
        Command, CommandErrorPayload,
        protocol::{self, ResponseEnvelope},
    };
    use tokio::{io::AsyncReadExt, net::UnixListener};

    use super::{ClientError, execute_unix};

    fn socket_path() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "focusguard-client-{}-{}.sock",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn unix_client_authenticates_server_and_checks_response_id() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = protocol::read_request_async(&mut stream).await.unwrap();
            let response = ResponseEnvelope::new(
                request.request_id,
                Err(CommandErrorPayload {
                    code: "probe_rejected".into(),
                    message: "probe".into(),
                }),
            );
            protocol::write_response_async(&mut stream, &response)
                .await
                .unwrap();
        });
        let server_uid = unsafe { libc::geteuid() };

        let result = execute_unix(&path, server_uid, Command::AppVersion).await;
        assert!(matches!(
            result,
            Err(ClientError::Service(CommandErrorPayload { ref code, .. }))
                if code == "probe_rejected"
        ));
        server.await.unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn unix_client_rejects_a_service_peer_with_the_wrong_uid() {
        let path = socket_path();
        let listener = UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut byte = [0; 1];
            stream.read(&mut byte).await.unwrap()
        });
        let peer_uid = unsafe { libc::geteuid() };
        let expected_uid = if peer_uid == 0 { 1 } else { 0 };

        let result = execute_unix(&path, expected_uid, Command::AppVersion).await;
        assert!(matches!(result, Err(ClientError::ServerIdentity(_))));
        assert_eq!(server.await.unwrap(), 0);
        let _ = std::fs::remove_file(&path);
    }
}
