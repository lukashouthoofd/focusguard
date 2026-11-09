//! FocusGuard Native Messaging Host
//!
//! Bridges the browser extension to FocusGuard via the Native Messaging protocol
//! (stdin/stdout length-prefixed JSON).
//!
//! ```text
//! Browser Extension <──stdio──> focuser-native <──IPC──> (no listener)
//! ```
//!
//! Extension rule requests use a persistent Native Messaging connection and the authenticated Windows service pipe.

mod ipc_client;

use std::{
    ffi::OsString,
    io::{self, Read, Write},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use focuser_common::extension::{ExtensionServiceEvent, NativeMessage, NativeMessageType};
use tracing::{debug, error, info};

const EXTENSION_IDENTITY_JSON: &str = include_str!("../../../extension/identity.json");

#[derive(Parser)]
#[command(
    name = "focuser-native",
    about = "FocusGuard Native Messaging host — bridges browser extension to service"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Register this binary as a Native Messaging host for browsers.
    Install {
        /// Browser extension ID (for Chrome/Chromium allowed_origins).
        #[arg(long)]
        extension_id: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    // Log to file instead of stdout (stdout is reserved for Native Messaging)
    let log_dir = directories::ProjectDirs::from("com", "publikhq", "FocusGuard")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(std::env::temp_dir);
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("focuser-native.log");

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .ok();

    if let Some(file) = log_file {
        tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .init();
    }

    let mut process_args = std::env::args_os();
    let executable = process_args.next().unwrap_or_default();
    let host_args: Vec<OsString> = process_args.collect();
    if matches!(
        host_args.first().and_then(|arg| arg.to_str()),
        Some("install" | "--help" | "--version")
    ) {
        let cli = Cli::parse_from(std::iter::once(executable).chain(host_args));
        if let Some(Commands::Install { extension_id }) = cli.command {
            install_native_host(&extension_id)?;
        }
        return Ok(());
    }

    validate_native_messaging_args(&host_args)?;
    run_native_host().await
}

fn validate_native_messaging_args(args: &[OsString]) -> Result<()> {
    let identity: serde_json::Value = serde_json::from_str(EXTENSION_IDENTITY_JSON)
        .context("parse the packaged browser extension identity")?;
    let chrome_id = identity["chrome_extension_id"]
        .as_str()
        .context("Chrome extension ID is missing from the packaged identity")?;
    let firefox_id = identity["firefox_extension_id"]
        .as_str()
        .context("Firefox extension ID is missing from the packaged identity")?;
    let values: Vec<&str> = args.iter().filter_map(|arg| arg.to_str()).collect();

    let chrome_origin = format!("chrome-extension://{chrome_id}/");
    if values.first() == Some(&chrome_origin.as_str()) {
        let valid_parent_window = |value: &str| {
            let window = value.strip_prefix("--parent-window=").unwrap_or(value);
            !window.is_empty() && window.bytes().all(|byte| byte.is_ascii_digit())
        };
        if values.len() == 1 || (values.len() == 2 && valid_parent_window(values[1])) {
            return Ok(());
        }
    }

    let firefox_manifest_path = values
        .first()
        .map(|path| path.replace('/', "\\").to_ascii_lowercase());
    let firefox_manifest_is_expected = firefox_manifest_path.is_some_and(|path| {
        path.ends_with("\\native-messaging\\com.focuser.native.firefox.json")
            || path.ends_with("\\native-messaging-hosts\\com.focuser.native.json")
            || path.ends_with("\\nativemessaginghosts\\com.focuser.native.json")
    });
    if values.len() == 2 && values[1] == firefox_id && firefox_manifest_is_expected {
        return Ok(());
    }

    anyhow::bail!(
        "native host launch did not come from the configured FocusGuard browser extension"
    )
}

/// Run the Native Messaging host loop.
async fn run_native_host() -> Result<()> {
    info!("FocusGuard native messaging host started");

    // Main loop: accept typed browser-host requests and call only the
    // authenticated service operation implemented for Native Messaging.
    loop {
        let msg = match read_native_message() {
            Ok(msg) => msg,
            Err(e) => {
                // EOF or read error — extension disconnected
                debug!(error = %e, "Failed to read native message, shutting down");
                break;
            }
        };

        debug!(msg_type = ?msg.msg_type, "Received native message");

        let response = match msg.msg_type {
            NativeMessageType::Ping => NativeMessage {
                msg_type: NativeMessageType::Pong,
                payload: serde_json::Value::Null,
                request_id: msg.request_id,
            },
            NativeMessageType::Event => {
                let event = serde_json::from_value::<ExtensionServiceEvent>(msg.payload.clone());
                match event {
                    Ok(event) => match ipc_client::send_extension_event(event).await {
                        Ok(()) => NativeMessage {
                            msg_type: NativeMessageType::Event,
                            payload: serde_json::json!({"ok": true}),
                            request_id: msg.request_id,
                        },
                        Err(error) => NativeMessage {
                            msg_type: NativeMessageType::Error,
                            payload: serde_json::Value::String(error.to_string()),
                            request_id: msg.request_id,
                        },
                    },
                    Err(error) => NativeMessage {
                        msg_type: NativeMessageType::Error,
                        payload: serde_json::Value::String(format!(
                            "invalid extension event: {error}"
                        )),
                        request_id: msg.request_id,
                    },
                }
            }
            NativeMessageType::RuleUpdate => match ipc_client::get_extension_rules().await {
                Ok(rules) => NativeMessage {
                    msg_type: NativeMessageType::RuleUpdate,
                    payload: serde_json::to_value(&rules).unwrap_or(serde_json::Value::Null),
                    request_id: msg.request_id,
                },
                Err(e) => NativeMessage {
                    msg_type: NativeMessageType::Error,
                    payload: serde_json::Value::String(e.to_string()),
                    request_id: msg.request_id,
                },
            },
            _ => NativeMessage {
                msg_type: NativeMessageType::Pong,
                payload: serde_json::Value::Null,
                request_id: msg.request_id,
            },
        };

        if let Err(e) = write_native_message(&response) {
            error!(error = %e, "Failed to write native message");
            break;
        }
    }

    info!("FocusGuard native messaging host stopped");
    Ok(())
}

// ─── Native Messaging Protocol ──────────────────────────────────────

/// Read a length-prefixed JSON message from stdin.
fn read_native_message() -> Result<NativeMessage> {
    let stdin = io::stdin();
    let mut handle = stdin.lock();

    // Read 4-byte little-endian length
    let mut len_buf = [0u8; 4];
    handle
        .read_exact(&mut len_buf)
        .context("Failed to read message length")?;
    let len = u32::from_le_bytes(len_buf) as usize;

    if len == 0 || len > 1024 * 1024 {
        anyhow::bail!("Invalid message length: {len}");
    }

    // Read JSON payload
    let mut buf = vec![0u8; len];
    handle
        .read_exact(&mut buf)
        .context("Failed to read message body")?;

    let msg: NativeMessage =
        serde_json::from_slice(&buf).context("Failed to parse native message JSON")?;
    Ok(msg)
}

/// Write a length-prefixed JSON message to stdout.
fn write_native_message(msg: &NativeMessage) -> Result<()> {
    let json = serde_json::to_vec(msg).context("Failed to serialize native message")?;
    let len = json.len() as u32;

    let stdout = io::stdout();
    let mut handle = stdout.lock();

    handle
        .write_all(&len.to_le_bytes())
        .context("Failed to write message length")?;
    handle
        .write_all(&json)
        .context("Failed to write message body")?;
    handle.flush().context("Failed to flush stdout")?;

    Ok(())
}

// ─── Browser Detection ──────────────────────────────────────────────

// ─── Native Messaging Host Installation ─────────────────────────────

/// Install the native messaging host manifest for supported browsers.
fn install_native_host(extension_id: &str) -> Result<()> {
    if !is_chrome_extension_id(extension_id) {
        anyhow::bail!(
            "--extension-id must be the exact 32-character Chrome extension ID (letters a-p)"
        );
    }
    let exe_path = std::env::current_exe().context("Cannot determine executable path")?;
    let exe_str = exe_path.to_string_lossy().to_string();

    println!("Installing FocusGuard native messaging host...");
    println!("Binary: {exe_str}");

    #[cfg(windows)]
    install_windows(&exe_str, extension_id)?;

    #[cfg(target_os = "linux")]
    install_linux(&exe_str, extension_id)?;

    #[cfg(target_os = "macos")]
    install_macos(&exe_str, extension_id)?;

    println!("Installation complete.");
    Ok(())
}

/// Generate the native messaging host manifest JSON.
fn host_manifest(exe_path: &str, extension_id: &str) -> String {
    format!(
        r#"{{
  "name": "com.focuser.native",
  "description": "FocusGuard Native Messaging Host",
  "path": "{}",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://{extension_id}/"]
}}"#,
        exe_path.replace('\\', "\\\\"),
    )
}

fn is_chrome_extension_id(extension_id: &str) -> bool {
    extension_id.len() == 32 && extension_id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

/// Firefox-specific manifest (uses allowed_extensions instead of allowed_origins).
fn host_manifest_firefox(exe_path: &str) -> String {
    format!(
        r#"{{
  "name": "com.focuser.native",
  "description": "FocusGuard Native Messaging Host",
  "path": "{}",
  "type": "stdio",
  "allowed_extensions": ["focusguard@publikhq.com"]
}}"#,
        exe_path.replace('\\', "\\\\"),
    )
}

#[cfg(windows)]
fn install_windows(exe_path: &str, extension_id: &str) -> Result<()> {
    use std::fs;

    // Write manifest files
    let data_dir = directories::ProjectDirs::from("com", "focuser", "FocusGuard")
        .context("Cannot determine data directory")?;
    let manifest_dir = data_dir.data_dir().join("native-messaging");
    fs::create_dir_all(&manifest_dir)?;

    let chrome_manifest = manifest_dir.join("com.focuser.native.json");
    let firefox_manifest = manifest_dir.join("com.focuser.native.firefox.json");

    fs::write(&chrome_manifest, host_manifest(exe_path, extension_id))?;
    fs::write(&firefox_manifest, host_manifest_firefox(exe_path))?;

    println!("Wrote Chrome manifest:  {}", chrome_manifest.display());
    println!("Wrote Firefox manifest: {}", firefox_manifest.display());

    // Register in Windows Registry
    // Chrome: HKCU\Software\Google\Chrome\NativeMessagingHosts\com.focuser.native
    // Edge:   HKCU\Software\Microsoft\Edge\NativeMessagingHosts\com.focuser.native
    // Firefox: HKCU\Software\Mozilla\NativeMessagingHosts\com.focuser.native
    let registry_entries = [
        (
            r"Software\Google\Chrome\NativeMessagingHosts\com.focuser.native",
            chrome_manifest.to_string_lossy().to_string(),
        ),
        (
            r"Software\Microsoft\Edge\NativeMessagingHosts\com.focuser.native",
            chrome_manifest.to_string_lossy().to_string(),
        ),
        (
            r"Software\Mozilla\NativeMessagingHosts\com.focuser.native",
            firefox_manifest.to_string_lossy().to_string(),
        ),
    ];

    for (key_path, manifest_path) in &registry_entries {
        match write_registry_key(key_path, manifest_path) {
            Ok(()) => println!("Registered: {key_path}"),
            Err(e) => eprintln!("Warning: Failed to register {key_path}: {e}"),
        }
    }

    Ok(())
}

#[cfg(windows)]
fn write_registry_key(key_path: &str, value: &str) -> Result<()> {
    use std::process::Command;

    // Use reg.exe to set the registry key (avoids winreg dependency)
    let output = Command::new("reg")
        .args([
            "add",
            &format!(r"HKCU\{key_path}"),
            "/ve",
            "/d",
            value,
            "/f",
        ])
        .output()
        .context("Failed to run reg.exe")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("reg.exe failed: {stderr}");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_linux(exe_path: &str, extension_id: &str) -> Result<()> {
    use std::fs;

    let home = std::env::var("HOME").context("HOME not set")?;

    // Chrome
    let chrome_dir = format!("{home}/.config/google-chrome/NativeMessagingHosts");
    fs::create_dir_all(&chrome_dir)?;
    fs::write(
        format!("{chrome_dir}/com.focuser.native.json"),
        host_manifest(exe_path, extension_id),
    )?;
    println!("Installed for Chrome: {chrome_dir}/com.focuser.native.json");

    // Chromium
    let chromium_dir = format!("{home}/.config/chromium/NativeMessagingHosts");
    fs::create_dir_all(&chromium_dir)?;
    fs::write(
        format!("{chromium_dir}/com.focuser.native.json"),
        host_manifest(exe_path, extension_id),
    )?;
    println!("Installed for Chromium: {chromium_dir}/com.focuser.native.json");

    // Firefox
    let firefox_dir = format!("{home}/.mozilla/native-messaging-hosts");
    fs::create_dir_all(&firefox_dir)?;
    fs::write(
        format!("{firefox_dir}/com.focuser.native.json"),
        host_manifest_firefox(exe_path),
    )?;
    println!("Installed for Firefox: {firefox_dir}/com.focuser.native.json");

    Ok(())
}

#[cfg(target_os = "macos")]
fn install_macos(exe_path: &str, extension_id: &str) -> Result<()> {
    use std::fs;

    let home = std::env::var("HOME").context("HOME not set")?;

    // Chrome
    let chrome_dir =
        format!("{home}/Library/Application Support/Google/Chrome/NativeMessagingHosts");
    fs::create_dir_all(&chrome_dir)?;
    fs::write(
        format!("{chrome_dir}/com.focuser.native.json"),
        host_manifest(exe_path, extension_id),
    )?;
    println!("Installed for Chrome: {chrome_dir}/com.focuser.native.json");

    // Firefox
    let firefox_dir = format!("{home}/Library/Application Support/Mozilla/NativeMessagingHosts");
    fs::create_dir_all(&firefox_dir)?;
    fs::write(
        format!("{firefox_dir}/com.focuser.native.json"),
        host_manifest_firefox(exe_path),
    )?;
    println!("Installed for Firefox: {firefox_dir}/com.focuser.native.json");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chrome_manifest_allows_only_the_supplied_extension_id() {
        let extension_id = "abcdefghijklmnopabcdefghijklmnop";
        assert!(is_chrome_extension_id(extension_id));
        let manifest = host_manifest(r"C:\FocusGuard\focuser-native.exe", extension_id);
        let manifest: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(
            manifest["allowed_origins"][0],
            format!("chrome-extension://{extension_id}/")
        );
        assert!(!manifest.to_string().contains("chrome-extension://*/"));
    }

    #[test]
    fn chrome_extension_id_must_be_an_exact_browser_id() {
        assert!(!is_chrome_extension_id(""));
        assert!(!is_chrome_extension_id("chrome-extension://*/"));
        assert!(!is_chrome_extension_id("abcdefghijklmnopabcdefghijklmnopq"));
        assert!(!is_chrome_extension_id("abcdefghijklmnopabcdefghijklmnox"));
    }

    #[test]
    fn native_host_accepts_only_the_pinned_chromium_and_firefox_launch_origins() {
        let chrome_id = "kiioaengbdebgaeifenloahfipolcndj";
        assert!(
            validate_native_messaging_args(&[
                OsString::from(format!("chrome-extension://{chrome_id}/")),
                OsString::from("--parent-window=0"),
            ])
            .is_ok()
        );
        assert!(validate_native_messaging_args(&[
            OsString::from(r"C:\Program Files\PublikHQ\FocusGuard\Service\native-messaging\com.focuser.native.firefox.json"),
            OsString::from("focusguard@publikhq.com"),
        ])
        .is_ok());
        assert!(validate_native_messaging_args(&[
            OsString::from(r"C:\Users\test\AppData\Roaming\focuser\FocusGuard\data\native-messaging\com.focuser.native.firefox.json"),
            OsString::from("focusguard@publikhq.com"),
        ])
        .is_ok());
        assert!(
            validate_native_messaging_args(&[
                OsString::from("chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/"),
                OsString::from("--parent-window=0"),
            ])
            .is_err()
        );
        assert!(validate_native_messaging_args(&[]).is_err());
    }
}
