#[cfg(windows)]
mod pipe;

#[cfg(windows)]
mod windows_service_host {
    use super::pipe;
    use std::{
        ffi::OsString,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use anyhow::{Context, bail};
    use tokio::{
        net::windows::named_pipe::NamedPipeServer,
        runtime::{Builder, Runtime},
        sync::oneshot,
    };
    use windows_service::{
        define_windows_service,
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
        service_dispatcher,
    };

    const SERVICE_NAME: &str = "FocusGuardService";
    const SERVICE_TYPE: ServiceType = ServiceType::OWN_PROCESS;

    define_windows_service!(ffi_service_main, service_main);

    pub fn run() -> anyhow::Result<()> {
        let args: Vec<OsString> = std::env::args_os().skip(1).collect();
        if args.first().is_some_and(|arg| arg == "--console") {
            let owner_sid = match parse_owner_sid(&args[1..])? {
                Some(sid) => sid,
                None => pipe::current_user_sid_string()?,
            };
            return run_console(owner_sid);
        }

        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .context("start FocusGuard service dispatcher")
    }

    fn parse_owner_sid(args: &[OsString]) -> anyhow::Result<Option<String>> {
        let mut parsed = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--owner-sid" {
                let sid = args.next().context("--owner-sid requires a SID")?;
                let sid = sid.to_string_lossy().into_owned();
                pipe::validate_sid(&sid)?;
                parsed = Some(sid);
            } else {
                bail!("unknown argument: {}", arg.to_string_lossy());
            }
        }
        Ok(parsed)
    }

    fn run_console(owner_sid: String) -> anyhow::Result<()> {
        let service_sid = pipe::current_user_sid_string()?;
        let context = Arc::new(pipe::new_context()?);
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        let listener = runtime
            .block_on(pipe::prepare(&owner_sid, &service_sid))
            .context("prepare secured pipe listener")?;
        runtime.block_on(async move {
            eprintln!(
                "FocusGuard service smoke host listening on {}; enforcement is not active.",
                pipe::PIPE_NAME
            );
            tokio::select! {
                result = pipe::serve(owner_sid, service_sid, listener, context) => result.context("named-pipe server stopped"),
                result = tokio::signal::ctrl_c() => result.context("wait for Ctrl+C"),
            }
        })
    }

    fn service_main(arguments: Vec<OsString>) {
        if let Err(error) = run_service(arguments) {
            eprintln!("FocusGuard service failed: {error:#}");
        }
    }

    fn run_service(arguments: Vec<OsString>) -> anyhow::Result<()> {
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let shutdown_tx = Arc::new(Mutex::new(Some(shutdown_tx)));
        let handler_tx = Arc::clone(&shutdown_tx);
        let status_handle =
            service_control_handler::register(SERVICE_NAME, move |event| match event {
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    if let Some(sender) = handler_tx.lock().expect("service stop lock").take() {
                        let _ = sender.send(());
                    }
                    ServiceControlHandlerResult::NoError
                }
                _ => ServiceControlHandlerResult::NotImplemented,
            })?;

        status_handle.set_service_status(status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            ServiceExitCode::NO_ERROR,
        ))?;
        let (owner_sid, service_sid, runtime, listener, context) = match prepare_service(arguments)
        {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = status_handle.set_service_status(status(
                    ServiceState::Stopped,
                    ServiceControlAccept::empty(),
                    ServiceExitCode::Win32(1),
                ));
                return Err(error);
            }
        };
        status_handle.set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            ServiceExitCode::NO_ERROR,
        ))?;

        let result = runtime.block_on(async move {
            tokio::select! {
                result = pipe::serve(owner_sid, service_sid, listener, context) => result.context("named-pipe server stopped"),
                _ = shutdown_rx => Ok(()),
            }
        });
        status_handle.set_service_status(status(
            ServiceState::StopPending,
            ServiceControlAccept::empty(),
            ServiceExitCode::NO_ERROR,
        ))?;
        let exit_code = if result.is_ok() {
            ServiceExitCode::NO_ERROR
        } else {
            ServiceExitCode::Win32(1)
        };
        status_handle.set_service_status(status(
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            exit_code,
        ))?;
        result
    }

    fn prepare_service(
        arguments: Vec<OsString>,
    ) -> anyhow::Result<(
        String,
        String,
        Runtime,
        NamedPipeServer,
        Arc<focuser_app::AppContext>,
    )> {
        let owner_sid = parse_owner_sid(&arguments)?
            .context("service configuration must include --owner-sid <SID>")?;
        let service_sid = pipe::service_sid_string(SERVICE_NAME)
            .context("resolve FocusGuard service SID; install with a service SID enabled")?;
        if !pipe::current_process_has_enabled_sid(&service_sid)? {
            bail!(
                "service SID is not enabled in the SCM process token; enable SERVICE_SID_TYPE_UNRESTRICTED or SERVICE_SID_TYPE_RESTRICTED"
            );
        }
        let context = Arc::new(pipe::new_context().map_err(|error| {
            anyhow::anyhow!("initialize command context before reporting Running: {error}")
        })?);
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        let listener = runtime
            .block_on(pipe::prepare(&owner_sid, &service_sid))
            .context("prepare secured pipe listener and verify service SID access")?;
        Ok((owner_sid, service_sid, runtime, listener, context))
    }

    fn status(
        current_state: ServiceState,
        controls_accepted: ServiceControlAccept,
        exit_code: ServiceExitCode,
    ) -> ServiceStatus {
        ServiceStatus {
            service_type: SERVICE_TYPE,
            current_state,
            controls_accepted,
            exit_code,
            checkpoint: 0,
            wait_hint: Duration::ZERO,
            process_id: None,
        }
    }
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_service_host::run()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("focusguard-service is only available on Windows");
}
