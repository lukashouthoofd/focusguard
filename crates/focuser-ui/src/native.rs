//! Native shims — the handful of operations that genuinely cannot live in
//! `focuser_app::execute`.
//!
//! Everything else goes through `run_command`. What is left here either needs a
//! Tauri handle (file dialogs, the updater) or enumerates OS processes, neither
//! of which belongs in a portable command core. Each one is deliberately thin:
//! it moves bytes or paths around and makes no product decisions.

use tauri_plugin_updater::UpdaterExt;

/// Pick an executable to block. Returns the name an `ExecutableName` rule
/// matches on, which is not always the file name — see `process::name_for_path`.
///
/// `(async)` is load-bearing. The plugin opens the dialog with
/// `run_on_main_thread`, so waiting for it *from* the main thread deadlocks and
/// the window hangs. That is what made Browse unusable on Linux.
#[tauri::command(async)]
pub fn pick_app_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let dialog = app.dialog().file().set_title("Select Application to Block");

    // Filters only where an executable actually carries an extension. rfd turns
    // every filter entry into `*.ext`, so on Linux even an "All files" filter
    // becomes `*.*` and hides every extensionless binary in /usr/bin.
    #[cfg(windows)]
    let dialog = dialog.add_filter("Programs", &["exe", "com", "bat", "cmd"]);
    #[cfg(target_os = "macos")]
    let dialog = dialog.add_filter("Applications", &["app"]);

    Ok(dialog
        .blocking_pick_file()
        .map(|picked| focuser_common::process::name_for_path(&picked.to_string())))
}

/// Pick a FocusGuard configuration file and return its contents.
/// `None` means the user cancelled. `(async)` for the same reason as above.
#[tauri::command(async)]
pub fn pick_import_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let Some(path) = app
        .dialog()
        .file()
        .set_title("Import FocusGuard Configuration")
        .add_filter("JSON", &["json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };
    std::fs::read_to_string(path.to_string())
        .map(Some)
        .map_err(|e| format!("Read failed: {e}"))
}

/// Ask where to save an exported configuration and write it there.
/// Returns the chosen path, or `None` if the user cancelled.
///
/// The document itself comes from `Command::ExportConfiguration` — this only
/// decides where it lands.
#[tauri::command(async)]
pub fn save_configuration(app: tauri::AppHandle, json: String) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;

    let default_name = format!(
        "focusguard-config-{}.json",
        chrono::Local::now().format("%Y-%m-%d")
    );
    let Some(path) = app
        .dialog()
        .file()
        .set_title("Export FocusGuard Configuration")
        .add_filter("JSON", &["json"])
        .set_file_name(&default_name)
        .blocking_save_file()
    else {
        return Ok(None);
    };
    let path = path.to_string();
    std::fs::write(&path, &json).map_err(|e| format!("Write failed: {e}"))?;
    Ok(Some(path))
}

/// Browser processes currently running, used by `SystemSync::running_browsers`.
pub fn detect_running_browsers() -> std::collections::HashSet<focuser_common::extension::BrowserType>
{
    focuser_common::process::list()
        .into_iter()
        .filter_map(|process| {
            focuser_common::browser::identify_browser(&process.name)
                .map(|browser| browser.browser_type.clone())
        })
        .collect()
}

#[tauri::command]
pub async fn check_for_update(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    match updater.check().await {
        Ok(Some(update)) => Ok(serde_json::json!({
            "available": true,
            "version": update.version,
            "body": update.body.unwrap_or_default(),
        })),
        Ok(None) => Ok(serde_json::json!({ "available": false })),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
pub async fn do_update(app: tauri::AppHandle) -> Result<(), String> {
    let updater = app.updater_builder().build().map_err(|e| e.to_string())?;
    let update = updater
        .check()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("No update available")?;
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Launch a specific browser at a URL — used to send someone to the extension
/// store page in the browser that actually needs it.
#[tauri::command]
pub fn open_browser_url(browser: String, url: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let output = macos_open_command(&browser, &url)?
            .output()
            .map_err(|e| format!("Failed to open {browser} at {url}: {e}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            return Err(format!(
                "Failed to open {browser} at {url}: {}",
                error.trim()
            ));
        }
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let exe_path = resolve_browser_exe(&browser);
        std::process::Command::new(&exe_path)
            .arg(&url)
            .spawn()
            .map_err(|e| format!("Failed to open {browser} at {exe_path}: {e}"))?;
        Ok(())
    }
}

#[cfg(any(target_os = "macos", test))]
fn resolve_browser_app(browser: &str) -> Option<&'static str> {
    match browser {
        "chrome" => Some("Google Chrome"),
        "firefox" => Some("Firefox"),
        "msedge" => Some("Microsoft Edge"),
        "brave" => Some("Brave Browser"),
        "opera" => Some("Opera"),
        _ => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn macos_open_command(browser: &str, url: &str) -> Result<std::process::Command, String> {
    let app =
        resolve_browser_app(browser).ok_or_else(|| format!("Unsupported browser: {browser}"))?;
    let mut command = std::process::Command::new("/usr/bin/open");
    command.args(["-a", app, url]);
    Ok(command)
}

/// Resolve a browser short name to its full executable path by querying the
/// Windows Registry `App Paths` key — the standard mechanism Windows uses to
/// locate installed applications regardless of install location.
#[cfg(windows)]
fn resolve_browser_exe(browser: &str) -> String {
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegOpenKeyExW,
        RegQueryValueExW,
    };
    use windows::core::PCWSTR;

    let exe_name = match browser {
        "chrome" => "chrome.exe",
        "firefox" => "firefox.exe",
        "msedge" => "msedge.exe",
        "brave" => "brave.exe",
        "opera" => "opera.exe",
        other => return other.to_string(),
    };

    let sub_key = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\{}",
        exe_name
    );
    let wide_key: Vec<u16> = sub_key.encode_utf16().chain(std::iter::once(0)).collect();

    for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut hkey = HKEY::default();
        let status =
            unsafe { RegOpenKeyExW(root, PCWSTR(wide_key.as_ptr()), 0, KEY_READ, &mut hkey) };
        if status.is_err() {
            continue;
        }

        let mut buf = vec![0u8; 1024];
        let mut buf_len = buf.len() as u32;

        let status = unsafe {
            RegQueryValueExW(
                hkey,
                PCWSTR::null(),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut buf_len),
            )
        };

        unsafe {
            let _ = RegCloseKey(hkey);
        }

        if status.is_ok() && buf_len > 2 {
            let wide_buf: Vec<u16> = buf[..buf_len as usize]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&c| u16::from_le_bytes(c))
                .collect();
            let len = wide_buf
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(wide_buf.len());
            let path = String::from_utf16_lossy(&wide_buf[..len]);
            if !path.is_empty() && std::path::Path::new(&path).exists() {
                return path;
            }
        }
    }

    if let Some(path) = find_exe_in_user_dirs(exe_name) {
        return path;
    }

    exe_name.to_string()
}

#[cfg(windows)]
fn find_exe_in_user_dirs(exe_name: &str) -> Option<String> {
    let local = std::env::var("LOCALAPPDATA").ok()?;
    let candidates: Vec<std::path::PathBuf> = match exe_name {
        "opera.exe" => vec![
            [&local, "Programs", "Opera", "opera.exe"].iter().collect(),
            [&local, "Programs", "Opera GX", "opera.exe"]
                .iter()
                .collect(),
        ],
        "brave.exe" => vec![
            [
                &local,
                "BraveSoftware",
                "Brave-Browser",
                "Application",
                "brave.exe",
            ]
            .iter()
            .collect(),
        ],
        "chrome.exe" => vec![
            [&local, "Google", "Chrome", "Application", "chrome.exe"]
                .iter()
                .collect(),
        ],
        _ => vec![],
    };
    candidates
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn resolve_browser_exe(browser: &str) -> String {
    browser.to_string()
}

#[cfg(test)]
mod macos_tests {
    use super::{macos_open_command, resolve_browser_app};

    #[test]
    fn browser_launch_names_resolve_to_macos_app_names() {
        assert_eq!(resolve_browser_app("chrome"), Some("Google Chrome"));
        assert_eq!(resolve_browser_app("firefox"), Some("Firefox"));
        assert_eq!(resolve_browser_app("msedge"), Some("Microsoft Edge"));
        assert_eq!(resolve_browser_app("brave"), Some("Brave Browser"));
        assert_eq!(resolve_browser_app("opera"), Some("Opera"));
        assert_eq!(resolve_browser_app("unknown"), None);
    }

    #[test]
    fn browser_launch_command_targets_the_selected_app_and_url() {
        let command = macos_open_command("chrome", "https://example.com/settings")
            .expect("supported browser should produce a command");
        assert_eq!(command.get_program(), "/usr/bin/open");
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["-a", "Google Chrome", "https://example.com/settings"]
        );
    }

    #[test]
    fn browser_launch_command_rejects_unknown_apps_without_running_them() {
        assert!(macos_open_command("unknown", "https://example.com").is_err());
    }
}
