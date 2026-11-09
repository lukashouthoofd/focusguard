//! The typed Tauri entry point into the command core.
//!
//! One command — [`run_command`] — takes a [`Command`] and returns a
//! [`CommandResult`]. Every transport therefore speaks the *same* shape:
//!
//! ```text
//!   Tauri webview  invoke("run_command", { command })  ─┐
//!   browser        POST /command                       ─┼─> execute()
//!   terminal       focuser <subcommand>                 ─┘
//! ```
//!
//! An earlier draft of this used 55 individual `#[tauri::command]` shims, which
//! is what tauri-specta is optimised for (per-command functions, JSDoc lifted
//! from Rust docs, per-command query keys). That was dropped: the dev server and
//! the CLI cannot use per-command Tauri bindings — they need a serialisable
//! envelope — so per-command shims would have meant maintaining two different
//! wire shapes for the same operations. One shape across all three transports is
//! worth more than the generated ergonomics, especially since the query hooks are
//! hand-written either way.
//!
//! specta still earns its place: it generates `Command`, `CommandResult`, and
//! `CommandError` as TypeScript, so the payload is fully checked and the
//! hand-maintained mirror in the frontend can be deleted.

use std::sync::Arc;

use focuser_app::{Command, CommandErrorPayload, CommandResult, execute};
use tauri::State;

use crate::AppState;

/// Selects the command owner for the current UI process. A registered SCM
/// service selects service mode even when it is stopped, so local mode never
/// starts a competing policy writer after service installation.
pub enum RuntimeBackend {
    Local(Arc<AppState>),
    Service,
    Unavailable(CommandErrorPayload),
}

/// Execute one command against the shared context.
///
/// Errors are flattened to [`CommandErrorPayload`] (`{ code, message }`) because
/// `CommandError` wraps `FocuserError`, which has no TypeScript representation.
/// The frontend branches on `code`, never on `message`.
/// `(async)` on a sync function puts it on Tauri's blocking thread pool.
/// Without it the default is the event-loop thread, and a slow command — icons
/// especially — freezes the window.
#[tauri::command]
#[specta::specta]
pub async fn run_command(
    backend: State<'_, RuntimeBackend>,
    command: Command,
) -> Result<CommandResult, CommandErrorPayload> {
    match backend.inner() {
        RuntimeBackend::Local(state) => tauri::async_runtime::spawn_blocking({
            let state = Arc::clone(state);
            move || execute(state.as_ref(), command)
        })
        .await
        .map_err(|error| CommandErrorPayload {
            code: "command_task_failed".into(),
            message: error.to_string(),
        })?
        .map_err(CommandErrorPayload::from),
        RuntimeBackend::Service => {
            #[cfg(any(windows, target_os = "macos"))]
            {
                focuser_client::execute(command)
                    .await
                    .map_err(service_client_error)
            }
            #[cfg(not(any(windows, target_os = "macos")))]
            {
                let _ = command;
                Err(CommandErrorPayload {
                    code: "service_backend_unsupported".into(),
                    message: "The service backend is only available on Windows and macOS.".into(),
                })
            }
        }
        RuntimeBackend::Unavailable(error) => Err(CommandErrorPayload {
            code: error.code.clone(),
            message: error.message.clone(),
        }),
    }
}

fn service_client_error(error: focuser_client::ClientError) -> CommandErrorPayload {
    use focuser_client::ClientError;

    let message = error.to_string();
    match error {
        ClientError::Service(payload) => payload,
        ClientError::Connect(_) => CommandErrorPayload {
            code: "service_unavailable".into(),
            message,
        },
        ClientError::ScmRegistration(_) => CommandErrorPayload {
            code: "service_registration_error".into(),
            message,
        },
        ClientError::ServiceRegistration(_) => CommandErrorPayload {
            code: "service_registration_error".into(),
            message,
        },
        ClientError::ServerIdentity(_) => CommandErrorPayload {
            code: "service_identity_error".into(),
            message,
        },
        ClientError::Timeout => CommandErrorPayload {
            code: "service_timeout".into(),
            message,
        },
        ClientError::InvalidResponse(_) | ClientError::Protocol(_) => CommandErrorPayload {
            code: "service_protocol_error".into(),
            message,
        },
        ClientError::UnsupportedPlatform => CommandErrorPayload {
            code: "service_backend_unsupported".into(),
            message,
        },
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::io;

    use super::service_client_error;
    use focuser_client::ClientError;

    #[test]
    fn scm_registration_errors_are_reported_to_the_ui() {
        let error =
            ClientError::ScmRegistration(io::Error::new(io::ErrorKind::PermissionDenied, "test"));
        let payload = service_client_error(error);

        assert_eq!(payload.code, "service_registration_error");
    }

    #[test]
    fn server_identity_errors_are_reported_to_the_ui() {
        let error =
            ClientError::ServerIdentity(io::Error::new(io::ErrorKind::PermissionDenied, "test"));
        let payload = service_client_error(error);

        assert_eq!(payload.code, "service_identity_error");
    }
}

/// Lets the frontend select the service-only screen without starting a
/// service request or guessing from local state.
#[tauri::command]
#[specta::specta]
pub fn is_service_mode(backend: State<'_, RuntimeBackend>) -> bool {
    !matches!(backend.inner(), RuntimeBackend::Local(_))
}

/// Current SMAppService authorization status. The command remains present on
/// other platforms so the frontend can keep one typed binding surface.
#[tauri::command]
pub fn macos_service_status() -> String {
    focuser_client::macos_service_status().to_owned()
}

/// Ask macOS to register the bundled privileged helper. Approval may still be
/// required in System Settings > General > Login Items.
#[tauri::command]
pub fn register_macos_service(app: tauri::AppHandle) -> Result<(), CommandErrorPayload> {
    focuser_client::register_macos_service().map_err(service_client_error)?;
    #[cfg(target_os = "macos")]
    {
        app.restart()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(())
    }
}

/// Open the macOS Login Items settings pane for service approval and control.
#[tauri::command]
pub fn open_macos_service_settings() -> Result<(), CommandErrorPayload> {
    focuser_client::open_macos_service_settings().map_err(service_client_error)
}

/// The specta builder, used both to export bindings and to document the surface.
///
/// Kept in one place so the export path and the registered commands cannot drift.
pub fn specta_builder() -> tauri_specta::Builder<tauri::Wry> {
    tauri_specta::Builder::<tauri::Wry>::new().commands(tauri_specta::collect_commands![
        run_command,
        is_service_mode
    ])
}

/// Where generated bindings are written, relative to the crate root.
const BINDINGS_PATH: &str = "frontend/src/bindings.ts";

/// Write `bindings.ts` next to the frontend source.
///
/// Invoked via `--export-bindings`, which exits before the database is opened or
/// a window is created. An earlier version did this during normal startup, but
/// that made regeneration depend on successfully launching a GUI app — which
/// fails silently when another instance holds the database, leaving stale
/// bindings behind with no error. A dedicated flag is deterministic and
/// scriptable:
///
/// ```text
/// cargo run -p focuser-ui -- --export-bindings
/// ```
///
/// Returns whether the export succeeded, so the caller can set an exit code.
pub fn export_bindings() -> bool {
    use specta_typescript::Typescript;

    // Note: 64-bit fields opt into `number` individually with
    // `#[specta(type = specta_typescript::Number)]` at their definition, rather
    // than being waved through globally here — so each one is a deliberate,
    // reviewable decision about precision.
    // Anchored to the crate directory, not the working directory — otherwise
    // where the bindings land depends on where you happened to run cargo from,
    // and `npm run bindings` (which runs from the frontend folder) would write
    // them into a second, unread copy of the tree.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(BINDINGS_PATH);

    match specta_builder().export(Typescript::default(), &path) {
        Ok(()) => {
            // Specta emits trailing spaces on blank lines and union members.
            // Keep generated output clean so whitespace checks and CI drift
            // checks agree on the canonical representation.
            let normalized = std::fs::read_to_string(&path)
                .map(|contents| {
                    let lines = contents.lines().map(str::trim_end).collect::<Vec<_>>();
                    format!("{}\n", lines.join("\n").trim_end())
                })
                .and_then(|contents| std::fs::write(&path, contents));
            if let Err(e) = normalized {
                eprintln!("failed to normalize bindings at {}: {e}", path.display());
                return false;
            }
            println!("exported TypeScript bindings to {}", path.display());
            true
        }
        Err(e) => {
            eprintln!("failed to export bindings to {}: {e}", path.display());
            false
        }
    }
}
