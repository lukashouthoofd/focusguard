//! Session- and owner-aware Windows process identities for service enforcement.
//!
//! A ToolHelp PID/name pair is not safe to act on later: the process may exit
//! and Windows may reuse its PID. These snapshots retain the owner SID, logon
//! session, image path, and creation time; termination opens a fresh handle and
//! verifies every field before acting on that process object.

use std::{collections::HashSet, ffi::c_void, io, mem::size_of, path::Path, ptr::null_mut};

use serde::{Deserialize, Serialize};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, FILETIME, HANDLE, LocalFree,
    },
    Security::Authorization::ConvertSidToStringSidW,
    Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenSessionId, TokenUser},
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        Threading::{
            GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess,
        },
    },
};

const MIN_USER_PID: u32 = 4;
const PROTECTED_PROCESS_NAMES: [&str; 4] = [
    "focusguard-ui.exe",
    "focusguard-service.exe",
    "focusguard.exe",
    // Reserved for the per-session window-message helper. The service must
    // never close or terminate its own UI-side enforcement bridge.
    "focusguard-agent.exe",
];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub session_id: u32,
    pub owner_sid: String,
    pub image_path: String,
    pub image_name: String,
    pub creation_time: u64,
}

impl ProcessIdentity {
    /// Whether this is one of FocusGuard's own processes. These executables
    /// are never valid app-block targets, including when observed from the
    /// separate SCM service process.
    pub fn is_focusguard_process(&self) -> bool {
        PROTECTED_PROCESS_NAMES
            .iter()
            .any(|name| self.image_name.eq_ignore_ascii_case(name))
    }
}

/// Snapshot processes whose query metadata is available to the service token.
/// Unknown owners or incomplete identities are omitted so enforcement can
/// never guess an account from a PID alone.
pub fn list_processes() -> Vec<ProcessIdentity> {
    let mut processes = Vec::new();
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return processes;
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = unsafe { Process32FirstW(snapshot.0, &mut entry) } != 0;
    while found {
        let pid = entry.th32ProcessID;
        if pid > MIN_USER_PID
            && let Ok(identity) = snapshot_process(pid)
        {
            processes.push(identity);
        }
        found = unsafe { Process32NextW(snapshot.0, &mut entry) } != 0;
    }
    processes
}

/// Return a process snapshot only when ToolHelp enumeration and every live
/// process identity query completed. A process that exited between the PID
/// listing and metadata query is safely skipped; any other query failure makes
/// the whole snapshot incomplete so enforcement cannot cancel or terminate
/// targets based on missing identities.
pub fn list_processes_complete() -> io::Result<Vec<ProcessIdentity>> {
    enumerate_processes_complete(None)
}

/// Return complete identities only for processes whose ToolHelp executable
/// name matches one of `executable_names`. Unrelated protected/system
/// processes are skipped without opening them; a query failure for any
/// matching live PID still makes the snapshot incomplete.
pub fn list_processes_matching_complete(
    executable_names: &[String],
) -> io::Result<Vec<ProcessIdentity>> {
    let names: HashSet<String> = executable_names
        .iter()
        .map(|name| name.to_lowercase())
        .collect();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    enumerate_processes_complete(Some(&names))
}

fn enumerate_processes_complete(
    matching_names: Option<&HashSet<String>>,
) -> io::Result<Vec<ProcessIdentity>> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot.is_null() || snapshot as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let snapshot = OwnedHandle(snapshot);
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            Ok(Vec::new())
        } else {
            Err(error)
        };
    }

    let mut processes = Vec::new();
    loop {
        let pid = entry.th32ProcessID;
        if pid > MIN_USER_PID
            && matching_names.is_none_or(|names| {
                process_entry_name(&entry.szExeFile)
                    .is_some_and(|name| names.contains(&name.to_lowercase()))
            })
        {
            match snapshot_process(pid) {
                Ok(identity) => processes.push(identity),
                // The process disappeared after ToolHelp captured its PID.
                // It cannot be a live target and is safe to omit.
                Err(error) if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) => {}
                Err(error) => return Err(error),
            }
        }

        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error);
        }
    }
    Ok(processes)
}

fn process_entry_name(buffer: &[u16]) -> Option<String> {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    (end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}

/// Read one process through a query-only handle. This is also useful for
/// checking a recorded target after a monitoring delay.
pub fn snapshot_process(pid: u32) -> io::Result<ProcessIdentity> {
    let handle = open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    identity_from_handle(pid, handle.0)
}

/// Terminate only if the live process still has the full identity recorded in
/// `expected` and belongs to the configured account. The open handle pins the
/// process object between validation and termination, preventing PID reuse
/// after the checks from redirecting the action.
pub fn terminate_if_unchanged(
    expected: &ProcessIdentity,
    configured_owner_sid: &str,
) -> io::Result<bool> {
    if expected.pid <= MIN_USER_PID
        || expected.pid == std::process::id()
        || expected.session_id == 0
        || expected.owner_sid != configured_owner_sid
        || expected.is_focusguard_process()
    {
        return Ok(false);
    }

    let handle = open_process(
        expected.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
    )?;
    let current = identity_from_handle(expected.pid, handle.0)?;
    if !same_process(expected, &current, configured_owner_sid) {
        return Ok(false);
    }

    let terminated = unsafe { TerminateProcess(handle.0, 1) } != 0;
    if terminated {
        Ok(true)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn same_process(
    expected: &ProcessIdentity,
    current: &ProcessIdentity,
    configured_owner_sid: &str,
) -> bool {
    expected.pid == current.pid
        && expected.session_id == current.session_id
        && expected.owner_sid == configured_owner_sid
        && current.owner_sid == configured_owner_sid
        && expected.creation_time == current.creation_time
        && expected
            .image_path
            .eq_ignore_ascii_case(&current.image_path)
        && expected
            .image_name
            .eq_ignore_ascii_case(&current.image_name)
}

fn identity_from_handle(pid: u32, process: HANDLE) -> io::Result<ProcessIdentity> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }

    // Query the session through the token tied to this open process handle.
    // ProcessIdToSessionId(pid) would look the PID up again and could observe
    // a different process if the original exits and Windows reuses its PID.
    let token = process_token(process)?;
    let session_id = process_session_id(token.0)?;

    let image_path = process_image_path(process)?;
    let image_name = Path::new(&image_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "process image has no name"))?;

    Ok(ProcessIdentity {
        pid,
        session_id,
        owner_sid: process_owner_sid(token.0)?,
        image_path,
        image_name,
        creation_time: (u64::from(creation.dwHighDateTime) << 32)
            | u64::from(creation.dwLowDateTime),
    })
}

fn process_image_path(process: HANDLE) -> io::Result<String> {
    let mut buffer = [0u16; 32768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(&buffer[..length as usize]))
}

fn process_token(process: HANDLE) -> io::Result<OwnedHandle> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(OwnedHandle(token))
}

fn process_session_id(token: HANDLE) -> io::Result<u32> {
    let mut session_id = 0u32;
    let mut returned = 0;
    if unsafe {
        GetTokenInformation(
            token,
            TokenSessionId,
            (&mut session_id as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut returned,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if returned as usize != size_of::<u32>() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process token returned an invalid session ID length",
        ));
    }
    Ok(session_id)
}

fn process_owner_sid(token: HANDLE) -> io::Result<String> {
    let mut required = 0;
    unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required) };
    if required == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut storage = vec![0usize; (required as usize).div_ceil(size_of::<usize>())];
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
    let mut sid_text = null_mut();
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

fn open_process(pid: u32, access: u32) -> io::Result<OwnedHandle> {
    let process = unsafe { OpenProcess(access, 0, pid) };
    if process.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(OwnedHandle(process))
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    use tempfile::TempDir;

    fn spawn_helper(directory: &TempDir) -> (Child, String) {
        let name = format!("focusguard-process-probe-{}.exe", uuid::Uuid::new_v4());
        let path = directory.path().join(&name);
        std::fs::copy(std::env::current_exe().unwrap(), &path).unwrap();
        let ready = directory.path().join("ready");
        let child = Command::new(path)
            .args([
                "--exact",
                "process_identity::tests::process_identity_child_probe",
                "--nocapture",
            ])
            .env("FOCUSGUARD_PROCESS_IDENTITY_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(ready.exists(), "probe process did not start");
        (child, name)
    }

    #[test]
    fn snapshot_contains_owner_session_image_and_creation_identity() {
        let current = snapshot_process(std::process::id()).unwrap();
        assert!(current.owner_sid.starts_with("S-1-"));
        assert!(current.image_name.to_ascii_lowercase().ends_with(".exe"));
        assert!(current.creation_time > 0);
    }

    #[test]
    fn process_identity_round_trips_for_helper_ipc() {
        let expected = snapshot_process(std::process::id()).unwrap();
        let encoded = serde_json::to_vec(&expected).unwrap();
        let decoded: ProcessIdentity = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded, expected);
    }

    #[test]
    fn process_enumeration_includes_the_current_test_process() {
        let current = list_processes()
            .into_iter()
            .find(|process| process.pid == std::process::id())
            .expect("process enumeration should include this test process");
        assert_eq!(current, snapshot_process(std::process::id()).unwrap());
    }

    #[test]
    fn complete_process_enumeration_includes_the_current_test_process() {
        let expected = snapshot_process(std::process::id()).unwrap();
        let current = list_processes_matching_complete(std::slice::from_ref(&expected.image_name))
            .unwrap()
            .into_iter()
            .find(|process| process.pid == std::process::id())
            .expect("matching complete enumeration should include this test process");
        assert_eq!(current, expected);
    }

    #[test]
    fn matching_complete_enumeration_skips_unrelated_processes() {
        let current = snapshot_process(std::process::id()).unwrap();
        let no_match = format!("focusguard-no-such-process-{}.exe", uuid::Uuid::new_v4());
        assert!(
            list_processes_matching_complete(&[no_match])
                .unwrap()
                .is_empty()
        );
        assert!(list_processes_matching_complete(&[]).unwrap().is_empty());
        assert!(!current.image_name.is_empty());
    }

    #[test]
    fn stale_creation_identity_is_never_terminated() {
        let directory = TempDir::new().unwrap();
        let (mut child, _) = spawn_helper(&directory);
        let mut identity = snapshot_process(child.id()).unwrap();
        let owner_sid = identity.owner_sid.clone();
        identity.creation_time += 1;

        assert!(!terminate_if_unchanged(&identity, &owner_sid).unwrap());
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn service_session_zero_and_focusguard_processes_are_never_terminated() {
        let directory = TempDir::new().unwrap();
        let (mut child, _) = spawn_helper(&directory);
        let mut identity = snapshot_process(child.id()).unwrap();
        let owner_sid = identity.owner_sid.clone();

        identity.session_id = 0;
        assert!(!terminate_if_unchanged(&identity, &owner_sid).unwrap());

        identity.session_id = snapshot_process(child.id()).unwrap().session_id;
        identity.image_name = "focusguard-ui.exe".into();
        assert!(!terminate_if_unchanged(&identity, &owner_sid).unwrap());
        identity.image_name = "focusguard-agent.exe".into();
        assert!(identity.is_focusguard_process());
        assert!(!terminate_if_unchanged(&identity, &owner_sid).unwrap());
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn configured_owner_mismatch_is_never_terminated() {
        let directory = TempDir::new().unwrap();
        let (mut child, _) = spawn_helper(&directory);
        let identity = snapshot_process(child.id()).unwrap();

        assert_ne!(identity.owner_sid, "S-1-0-0");
        assert!(!terminate_if_unchanged(&identity, "S-1-0-0").unwrap());
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn matching_identity_terminates_only_the_sacrificial_child_process() {
        let directory = TempDir::new().unwrap();
        let (mut child, expected_name) = spawn_helper(&directory);
        let identity = snapshot_process(child.id()).unwrap();
        let owner_sid = identity.owner_sid.clone();

        assert_eq!(identity.image_name, expected_name);
        assert_eq!(
            identity.session_id,
            snapshot_process(std::process::id()).unwrap().session_id
        );
        assert!(terminate_if_unchanged(&identity, &owner_sid).unwrap());

        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            child.try_wait().unwrap().is_some(),
            "target process survived termination"
        );
    }

    #[test]
    fn process_identity_child_probe() {
        let Ok(ready) = std::env::var("FOCUSGUARD_PROCESS_IDENTITY_READY") else {
            return;
        };
        std::fs::write(ready, std::process::id().to_string()).unwrap();
        loop {
            thread::sleep(Duration::from_secs(60));
        }
    }
}
