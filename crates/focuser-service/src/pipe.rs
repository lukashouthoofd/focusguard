use std::{
    ffi::c_void,
    io,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, RawHandle},
    },
    ptr::null_mut,
    sync::Arc,
    time::Duration,
};

use focuser_app::{
    AppContext, Command, CommandErrorPayload,
    protocol::{self, RequestEnvelope, ResponseEnvelope},
};
use focuser_core::{BlockEngine, Database};
use tokio::{
    io::AsyncWriteExt, net::windows::named_pipe::NamedPipeServer, sync::Semaphore, task::JoinSet,
    time::timeout,
};
use windows_sys::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree},
        Security::Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            SDDL_REVISION_1,
        },
        Security::{
            CheckTokenMembership, DuplicateTokenEx, GetTokenInformation, LookupAccountNameW,
            RevertToSelf, SECURITY_ATTRIBUTES, SecurityImpersonation, TOKEN_DUPLICATE, TOKEN_QUERY,
            TOKEN_USER, TokenImpersonation, TokenUser,
        },
        Storage::FileSystem::{
            FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_READ_ATTRIBUTES,
            FILE_READ_DATA, FILE_READ_EA, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
            PIPE_ACCESS_DUPLEX, READ_CONTROL, SYNCHRONIZE,
        },
        System::{
            Pipes::{
                CreateNamedPipeW, ImpersonateNamedPipeClient, PIPE_READMODE_BYTE,
                PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
            },
            Threading::{GetCurrentProcess, GetCurrentThread, OpenProcessToken, OpenThreadToken},
        },
    },
    core::PWSTR,
};

pub const PIPE_NAME: &str = r"\\.\pipe\PublikHQ.FocusGuard.Service.v1";
const MAX_PIPE_INSTANCES: u32 = 4;
const MAX_CLIENTS: usize = MAX_PIPE_INSTANCES as usize - 1;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
const PIPE_CLIENT_RIGHTS: u32 = FILE_READ_DATA
    | FILE_WRITE_DATA
    | FILE_READ_ATTRIBUTES
    | FILE_WRITE_ATTRIBUTES
    | FILE_READ_EA
    | FILE_WRITE_EA
    | READ_CONTROL
    | SYNCHRONIZE;
const PIPE_FULL_CONTROL: u32 = 0x001F01FF;

pub async fn prepare(owner_sid: &str, service_sid: &str) -> io::Result<NamedPipeServer> {
    prepare_named(PIPE_NAME, owner_sid, service_sid).await
}

async fn prepare_named(
    name: &str,
    owner_sid: &str,
    service_sid: &str,
) -> io::Result<NamedPipeServer> {
    validate_sid(owner_sid)?;
    validate_sid(service_sid)?;
    let listener = create_pipe(name, owner_sid, service_sid, true)?;
    // Create a second instance during startup. Besides checking that the
    // service-specific SID is present in the SCM token, this catches ACL or
    // account configuration failures before the SCM reports Running.
    let instance_probe = create_pipe(name, owner_sid, service_sid, false)?;
    drop(instance_probe);
    Ok(listener)
}

pub async fn serve(
    owner_sid: String,
    service_sid: String,
    mut listener: NamedPipeServer,
    context: Arc<AppContext>,
    policy_enabled: bool,
) -> io::Result<()> {
    validate_sid(&owner_sid)?;
    validate_sid(&service_sid)?;
    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut tasks = JoinSet::new();

    loop {
        listener.connect().await?;
        let permit = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("pipe server is shutting down"))?;

        // Keep an instance listening before the connected client is dispatched.
        let next_listener = create_pipe(PIPE_NAME, &owner_sid, &service_sid, false)?;
        let connected = listener;
        listener = next_listener;
        let client_context = Arc::clone(&context);
        let client_owner_sid = owner_sid.clone();
        let client_policy_enabled = policy_enabled;
        tasks.spawn(async move {
            let _permit = permit;
            if let Err(error) = handle_connection(
                connected,
                &client_owner_sid,
                &client_context,
                client_policy_enabled,
            )
            .await
            {
                tracing::debug!(%error, "rejected or failed named-pipe request");
            }
        });
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::warn!(%error, "named-pipe request task panicked");
            }
        }
    }
}

pub fn new_context() -> io::Result<AppContext> {
    let database = Database::open_in_memory().map_err(other_io)?;
    let engine = BlockEngine::new(database).map_err(other_io)?;
    Ok(AppContext::new_headless(engine))
}

async fn handle_connection(
    mut pipe: NamedPipeServer,
    owner_sid: &str,
    context: &AppContext,
    policy_enabled: bool,
) -> io::Result<()> {
    let request: RequestEnvelope = timeout(CLIENT_TIMEOUT, protocol::read_request_async(&mut pipe))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request frame deadline elapsed"))??;

    let peer_sid = impersonated_client_sid(pipe.as_raw_handle() as HANDLE)?;
    if peer_sid != owner_sid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "named-pipe caller SID is not the configured owner",
        ));
    }

    let response = if matches!(&request.command, Command::AppVersion)
        || (policy_enabled && service_policy_command_allowed(&request.command))
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
                    "This service build only supports exact-domain policy commands.".into()
                } else {
                    "The Windows service smoke host currently supports only its version probe."
                        .into()
                },
            }),
        )
    };
    timeout(
        CLIENT_TIMEOUT,
        protocol::write_response_async(&mut pipe, &response),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response write deadline elapsed"))??;
    pipe.flush().await
}

/// The first policy-enabled service mode owns durable exact-domain hosts
/// enforcement. Other features remain on the desktop until their workers move
/// behind the service boundary too.
fn service_policy_command_allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::ListBlockLists
            | Command::CreateBlockList { .. }
            | Command::DeleteBlockList { .. }
            | Command::ToggleBlockList { .. }
            | Command::AddWebsiteRule { .. }
            | Command::RemoveWebsiteRule { .. }
            | Command::BulkImportWebsites { .. }
            | Command::ClearAllWebsites
            | Command::UpdateSchedule { .. }
            | Command::GetBlockingHealth
            | Command::ApplyBlocks
            | Command::RemoveBlocks
    )
}

fn create_pipe(
    name: &str,
    owner_sid: &str,
    service_sid: &str,
    first: bool,
) -> io::Result<NamedPipeServer> {
    let (descriptor, attributes) = security_attributes(owner_sid, service_sid)?;
    let name = wide(name);
    let mut open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
    }
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            open_mode,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_REJECT_REMOTE_CLIENTS,
            MAX_PIPE_INSTANCES,
            64 * 1024,
            64 * 1024,
            0,
            &attributes,
        )
    };
    unsafe { LocalFree(descriptor.cast()) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    unsafe { NamedPipeServer::from_raw_handle(handle as RawHandle) }
}

fn security_attributes(
    owner_sid: &str,
    service_sid: &str,
) -> io::Result<(*mut c_void, SECURITY_ATTRIBUTES)> {
    validate_sid(owner_sid)?;
    validate_sid(service_sid)?;
    let sddl = wide(format!(
        "D:P(A;;0x{PIPE_CLIENT_RIGHTS:08X};;;{owner_sid})(A;;0x{PIPE_FULL_CONTROL:08X};;;{service_sid})(A;;0x{PIPE_FULL_CONTROL:08X};;;SY)(A;;0x{PIPE_FULL_CONTROL:08X};;;BA)"
    ));
    let mut descriptor = null_mut();
    let result = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    Ok((descriptor, attributes))
}

pub fn validate_sid(sid: &str) -> io::Result<()> {
    if !sid.starts_with("S-")
        || !sid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'S' || byte == b'-')
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "owner SID must use canonical SDDL SID characters",
        ));
    }
    let sid = wide(sid);
    let mut parsed_sid = null_mut();
    let result = unsafe {
        windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW(
            sid.as_ptr(),
            &mut parsed_sid,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { LocalFree(parsed_sid) };
    Ok(())
}

fn impersonated_client_sid(pipe: HANDLE) -> io::Result<String> {
    // Keep impersonation off Tokio's reusable worker threads. If reverting the
    // token fails, panic only this dedicated thread so its impersonation token
    // cannot leak into unrelated work on a pooled executor thread.
    let pipe = pipe as usize;
    std::thread::spawn(move || {
        let pipe = pipe as HANDLE;
        if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid_result = unsafe {
            let mut token = null_mut();
            if OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) == 0 {
                Err(io::Error::last_os_error())
            } else {
                let token = OwnedHandle(token);
                token_user_sid(token.0)
            }
        };
        if unsafe { RevertToSelf() } == 0 {
            panic!("RevertToSelf failed; terminating impersonated worker thread");
        }
        sid_result
    })
    .join()
    .map_err(|_| io::Error::other("impersonated worker thread failed"))?
}

fn current_user_sid() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);
    unsafe { token_user_sid(token.0) }
}

unsafe fn token_user_sid(token: HANDLE) -> io::Result<String> {
    let mut required = 0;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required) };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let words = (required as usize).div_ceil(size_of::<usize>());
    let mut storage = vec![0usize; words];
    if unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            storage.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let token_user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
    sid_to_string(token_user.User.Sid)
}

fn sid_to_string(sid: *mut c_void) -> io::Result<String> {
    let mut sid_text: PWSTR = null_mut();
    if unsafe { ConvertSidToStringSidW(sid, &mut sid_text) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut length = 0;
    unsafe {
        while *sid_text.add(length) != 0 {
            length += 1;
        }
    }
    let text = unsafe { std::slice::from_raw_parts(sid_text, length) };
    let sid =
        String::from_utf16(text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
    unsafe { LocalFree(sid_text.cast()) };
    sid
}

fn wide(value: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn other_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub fn current_user_sid_string() -> io::Result<String> {
    current_user_sid()
}

pub fn service_sid_string(service_name: &str) -> io::Result<String> {
    let account = wide(format!("NT SERVICE\\{service_name}"));
    let mut sid_size = 0;
    let mut domain_size = 0;
    let mut sid_name_use = 0;
    unsafe {
        LookupAccountNameW(
            null_mut(),
            account.as_ptr(),
            null_mut(),
            &mut sid_size,
            null_mut(),
            &mut domain_size,
            &mut sid_name_use,
        )
    };
    if sid_size == 0 || domain_size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut sid_buffer = vec![0usize; (sid_size as usize).div_ceil(size_of::<usize>())];
    let mut domain_buffer = vec![0u16; domain_size as usize];
    if unsafe {
        LookupAccountNameW(
            null_mut(),
            account.as_ptr(),
            sid_buffer.as_mut_ptr().cast(),
            &mut sid_size,
            domain_buffer.as_mut_ptr(),
            &mut domain_size,
            &mut sid_name_use,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    sid_to_string(sid_buffer.as_mut_ptr().cast())
}

pub fn current_process_has_enabled_sid(sid: &str) -> io::Result<bool> {
    let sid = wide(sid);
    let mut sid_pointer = null_mut();
    if unsafe {
        windows_sys::Win32::Security::Authorization::ConvertStringSidToSidW(
            sid.as_ptr(),
            &mut sid_pointer,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    let mut token = null_mut();
    if unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY | TOKEN_DUPLICATE,
            &mut token,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        unsafe { LocalFree(sid_pointer) };
        return Err(error);
    }
    let token = OwnedHandle(token);
    let mut impersonation_token = null_mut();
    if unsafe {
        DuplicateTokenEx(
            token.0,
            TOKEN_QUERY,
            null_mut(),
            SecurityImpersonation,
            TokenImpersonation,
            &mut impersonation_token,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        unsafe { LocalFree(sid_pointer) };
        return Err(error);
    }
    let impersonation_token = OwnedHandle(impersonation_token);
    let mut is_member = 0;
    let check = unsafe { CheckTokenMembership(impersonation_token.0, sid_pointer, &mut is_member) };
    let check_error = (check == 0).then(io::Error::last_os_error);
    unsafe { LocalFree(sid_pointer) };
    if let Some(error) = check_error {
        return Err(error);
    }
    Ok(is_member != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::ptr::null;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use focuser_app::{CommandResult, protocol::RequestEnvelope};
    use focuser_common::types::WebsiteMatchType;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_CREATE_PIPE_INSTANCE, OPEN_EXISTING, SECURITY_IDENTIFICATION,
        SECURITY_SQOS_PRESENT,
    };

    static NEXT_TEST_PIPE: AtomicU32 = AtomicU32::new(0);

    fn test_pipe_name() -> String {
        format!(
            r"\\.\pipe\FocusGuard.Test.{}.{}",
            std::process::id(),
            NEXT_TEST_PIPE.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn open_test_client(
        name: &str,
    ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        let name = wide(name);
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                PIPE_CLIENT_RIGHTS,
                0,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        unsafe {
            tokio::net::windows::named_pipe::NamedPipeClient::from_raw_handle(handle as RawHandle)
        }
    }

    fn no_flush() -> io::Result<()> {
        Ok(())
    }

    async fn send_policy_command(
        name: &str,
        owner_sid: &str,
        service_sid: &str,
        context: Arc<AppContext>,
        command: Command,
    ) -> ResponseEnvelope {
        let server = create_pipe(name, owner_sid, service_sid, true).unwrap();
        let owner_sid = owner_sid.to_owned();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context, true).await
        });

        let mut client = open_test_client(name).unwrap();
        let request = RequestEnvelope::new(uuid::Uuid::new_v4(), command);
        protocol::write_request_async(&mut client, &request)
            .await
            .unwrap();
        let response = protocol::read_response_async(&mut client).await.unwrap();
        server_task.await.unwrap().unwrap();
        response
    }

    #[tokio::test]
    async fn policy_pipe_persists_and_reapplies_exact_domain_hosts_rules() {
        let name = test_pipe_name();
        let owner_sid = current_user_sid_string().unwrap();
        let service_sid = current_user_sid_string().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("service.db");
        let hosts = directory.path().join("hosts");
        let original = "# user entry\r\n127.0.0.1 localhost\r\n";
        fs::write(&hosts, original).unwrap();
        let context =
            Arc::new(crate::enforcement::open_context(&database, &hosts, no_flush).unwrap());

        let response = send_policy_command(
            &name,
            &owner_sid,
            &service_sid,
            Arc::clone(&context),
            Command::CreateBlockList {
                name: "Work".into(),
            },
        )
        .await;
        let list = response.result.unwrap().as_block_list().unwrap().clone();
        send_policy_command(
            &name,
            &owner_sid,
            &service_sid,
            Arc::clone(&context),
            Command::ToggleBlockList {
                id: list.id,
                enabled: true,
            },
        )
        .await
        .result
        .unwrap();
        send_policy_command(
            &name,
            &owner_sid,
            &service_sid,
            Arc::clone(&context),
            Command::AddWebsiteRule {
                list_id: list.id,
                rule: WebsiteMatchType::Domain("reddit.com".into()),
            },
        )
        .await
        .result
        .unwrap();

        let applied = fs::read_to_string(&hosts).unwrap();
        assert!(applied.starts_with(original));
        assert!(applied.contains("127.0.0.1 reddit.com\r\n"));

        drop(context);
        let reopened =
            Arc::new(crate::enforcement::open_context(&database, &hosts, no_flush).unwrap());
        crate::enforcement::reconcile_once(&reopened).unwrap();
        let recovered = fs::read_to_string(&hosts).unwrap();
        assert!(recovered.contains("127.0.0.1 reddit.com\r\n"));
        assert_eq!(recovered, applied);
    }

    #[tokio::test]
    async fn secured_pipe_round_trips_only_the_smoke_probe() {
        let name = test_pipe_name();
        let owner_sid = current_user_sid().unwrap();
        let service_sid = current_user_sid().unwrap();
        let server = create_pipe(&name, &owner_sid, &service_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context, false).await
        });
        let mut client = open_test_client(&name).unwrap();
        let request = RequestEnvelope::new(uuid::Uuid::new_v4(), Command::AppVersion);
        let request_id = request.request_id;
        protocol::write_request_async(&mut client, &request)
            .await
            .unwrap();
        let response = protocol::read_response_async(&mut client).await.unwrap();
        assert_eq!(response.request_id, request_id);
        assert!(matches!(response.result, Ok(CommandResult::Text(_))));
        server_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn secured_pipe_rejects_mutations_and_bad_frames() {
        let name = test_pipe_name();
        let owner_sid = current_user_sid().unwrap();
        let service_sid = current_user_sid().unwrap();
        let server = create_pipe(&name, &owner_sid, &service_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context, false).await
        });
        let mut client = open_test_client(&name).unwrap();
        let request = RequestEnvelope::new(uuid::Uuid::new_v4(), Command::DeleteAllData);
        protocol::write_request_async(&mut client, &request)
            .await
            .unwrap();
        let response = protocol::read_response_async(&mut client).await.unwrap();
        assert!(matches!(
            response.result,
            Err(CommandErrorPayload { ref code, .. }) if code == "service_smoke_only"
        ));
        server_task.await.unwrap().unwrap();

        let name = test_pipe_name();
        let owner_sid = current_user_sid().unwrap();
        let service_sid = current_user_sid().unwrap();
        let server = create_pipe(&name, &owner_sid, &service_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context, false).await
        });
        let mut client = open_test_client(&name).unwrap();
        client
            .write_all(&((protocol::MAX_FRAME_BYTES as u32 + 1).to_be_bytes()))
            .await
            .unwrap();
        let mut response = [0; 1];
        assert!(matches!(client.read(&mut response).await, Ok(0) | Err(_)));
        assert_eq!(
            server_task.await.unwrap().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn pipe_rejects_a_different_user_sid_after_connection() {
        let name = test_pipe_name();
        let current_sid = current_user_sid().unwrap();
        if current_sid == "S-1-5-18" {
            return;
        }
        let owner_sid = "S-1-5-18".to_owned();
        let service_sid = current_user_sid().unwrap();
        let server = create_pipe(&name, &owner_sid, &service_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context, false).await
        });
        let mut client = open_test_client(&name).unwrap();
        let request = RequestEnvelope::new(uuid::Uuid::new_v4(), Command::AppVersion);
        protocol::write_request_async(&mut client, &request)
            .await
            .unwrap();
        assert!(matches!(client.read(&mut [0; 1]).await, Ok(0) | Err(_)));
        assert_eq!(
            server_task.await.unwrap().unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn first_pipe_instance_prevents_name_squatting() {
        let name = test_pipe_name();
        let owner_sid = current_user_sid().unwrap();
        let service_sid = current_user_sid().unwrap();
        let _server = create_pipe(&name, &owner_sid, &service_sid, true).unwrap();
        let service_sid = current_user_sid().unwrap();
        let error = create_pipe(&name, &owner_sid, &service_sid, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[tokio::test]
    async fn startup_prepares_listener_and_verifies_instance_access() {
        let name = test_pipe_name();
        let owner_sid = current_user_sid().unwrap();
        let service_sid = current_user_sid().unwrap();
        let listener = prepare_named(&name, &owner_sid, &service_sid)
            .await
            .unwrap();
        assert_eq!(listener.info().unwrap().max_instances, MAX_PIPE_INSTANCES);
    }

    #[test]
    fn owner_sid_is_validated_before_sddl_construction() {
        assert!(validate_sid("S-1-5-18").is_ok());
        assert_eq!(
            validate_sid("S-1-5-18);(A;;GA;;;WD").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(PIPE_CLIENT_RIGHTS & FILE_CREATE_PIPE_INSTANCE, 0);
    }

    #[test]
    fn membership_check_uses_a_queryable_impersonation_token() {
        assert!(current_process_has_enabled_sid("S-1-1-0").unwrap());
    }
}
