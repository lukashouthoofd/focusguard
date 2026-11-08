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
            GetTokenInformation, RevertToSelf, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
            TokenUser,
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

pub async fn serve(owner_sid: String) -> io::Result<()> {
    validate_sid(&owner_sid)?;
    let context = Arc::new(new_context()?);
    let slots = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut tasks = JoinSet::new();
    let mut first = true;
    let mut listener = create_pipe(PIPE_NAME, &owner_sid, first)?;

    loop {
        listener.connect().await?;
        let permit = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|_| io::Error::other("pipe server is shutting down"))?;

        // Keep an instance listening before the connected client is dispatched.
        first = false;
        let next_listener = create_pipe(PIPE_NAME, &owner_sid, first)?;
        let connected = listener;
        listener = next_listener;
        let client_context = Arc::clone(&context);
        let client_owner_sid = owner_sid.clone();
        tasks.spawn(async move {
            let _permit = permit;
            if let Err(error) =
                handle_connection(connected, &client_owner_sid, &client_context).await
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

fn new_context() -> io::Result<AppContext> {
    let database = Database::open_in_memory().map_err(other_io)?;
    let engine = BlockEngine::new(database).map_err(other_io)?;
    Ok(AppContext::new_headless(engine))
}

async fn handle_connection(
    mut pipe: NamedPipeServer,
    owner_sid: &str,
    context: &AppContext,
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

    // This first milestone is deliberately a probe-only host. Do not forward
    // mutations into the headless NoopSync context and imply that they enforce.
    let response = if matches!(request.command, Command::AppVersion) {
        protocol::dispatch_service_request(context, request)
    } else {
        ResponseEnvelope::new(
            request.request_id,
            Err(CommandErrorPayload {
                code: "service_smoke_only".into(),
                message: "The Windows service host currently supports only its version probe."
                    .into(),
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

fn create_pipe(name: &str, owner_sid: &str, first: bool) -> io::Result<NamedPipeServer> {
    let service_sid = current_user_sid()?;
    let (descriptor, attributes) = security_attributes(owner_sid, &service_sid)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null;
    use std::sync::atomic::{AtomicU32, Ordering};

    use focuser_app::CommandResult;
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

    #[tokio::test]
    async fn secured_pipe_round_trips_only_the_smoke_probe() {
        let name = test_pipe_name();
        let owner_sid = current_user_sid().unwrap();
        let server = create_pipe(&name, &owner_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context).await
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
        let server = create_pipe(&name, &owner_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context).await
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
        let server = create_pipe(&name, &owner_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context).await
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
        let server = create_pipe(&name, &owner_sid, true).unwrap();
        let context = new_context().unwrap();
        let server_task = tokio::spawn(async move {
            let server = server;
            server.connect().await?;
            handle_connection(server, &owner_sid, &context).await
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
        let _server = create_pipe(&name, &owner_sid, true).unwrap();
        let error = create_pipe(&name, &owner_sid, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
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
}
