#[cfg(windows)]
mod pipe;

#[cfg(windows)]
mod enforcement;

#[cfg(windows)]
mod windows_service_host {
    use super::enforcement;
    use super::pipe;
    use std::{
        ffi::OsString,
        path::PathBuf,
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
            let options = parse_options(&args[1..])?;
            let owner_sid = match options.owner_sid {
                Some(sid) => sid,
                None => pipe::current_user_sid_string()?,
            };
            let service_sid = pipe::current_user_sid_string()?;
            let (context, policy_enabled) = match (options.database_path, options.hosts_path) {
                (Some(database), Some(hosts)) => (
                    Arc::new(enforcement::open_context(
                        database,
                        hosts,
                        enforcement::flush_system_dns,
                    )?),
                    true,
                ),
                (None, None) => (Arc::new(pipe::new_context()?), false),
                _ => bail!("--database and --hosts-file must be supplied together"),
            };
            return run_console(owner_sid, service_sid, context, policy_enabled);
        }

        service_dispatcher::start(SERVICE_NAME, ffi_service_main)
            .context("start FocusGuard service dispatcher")
    }

    #[derive(Default)]
    struct ServiceOptions {
        owner_sid: Option<String>,
        database_path: Option<PathBuf>,
        hosts_path: Option<PathBuf>,
    }

    fn parse_options(args: &[OsString]) -> anyhow::Result<ServiceOptions> {
        let mut parsed = ServiceOptions::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--owner-sid" {
                let sid = args.next().context("--owner-sid requires a SID")?;
                let sid = sid.to_string_lossy().into_owned();
                pipe::validate_sid(&sid)?;
                if parsed.owner_sid.replace(sid).is_some() {
                    bail!("--owner-sid may only be supplied once");
                }
            } else if arg == "--database" {
                let path = args.next().context("--database requires a path")?;
                if parsed.database_path.replace(PathBuf::from(path)).is_some() {
                    bail!("--database may only be supplied once");
                }
            } else if arg == "--hosts-file" {
                let path = args.next().context("--hosts-file requires a path")?;
                if parsed.hosts_path.replace(PathBuf::from(path)).is_some() {
                    bail!("--hosts-file may only be supplied once");
                }
            } else {
                bail!("unknown argument: {}", arg.to_string_lossy());
            }
        }
        if parsed.database_path.is_some() != parsed.hosts_path.is_some() {
            bail!("--database and --hosts-file must be supplied together");
        }
        Ok(parsed)
    }

    fn run_console(
        owner_sid: String,
        service_sid: String,
        context: Arc<focuser_app::AppContext>,
        policy_enabled: bool,
    ) -> anyhow::Result<()> {
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        let listener = runtime
            .block_on(pipe::prepare(&owner_sid, &service_sid))
            .context("prepare secured pipe listener")?;
        if policy_enabled && let Err(error) = enforcement::reconcile_once(&context) {
            tracing::warn!(%error, "initial console policy reconciliation failed");
        }
        let reconcile_context = Arc::clone(&context);
        runtime.block_on(async move {
            eprintln!(
                "FocusGuard service {} listening on {}.",
                if policy_enabled {
                    "domain policy host"
                } else {
                    "smoke host; enforcement is not active"
                },
                pipe::PIPE_NAME,
            );
            tokio::select! {
                result = pipe::serve(owner_sid, service_sid, listener, context, policy_enabled) => result.context("named-pipe server stopped"),
                _ = async { if policy_enabled { enforcement::run_reconciler(reconcile_context).await } else { std::future::pending::<()>().await } } => Ok(()),
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

        report_start_pending(&status_handle, 1)?;
        let (owner_sid, service_sid, runtime, listener, context) =
            match prepare_service(arguments, &status_handle) {
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

        let reconcile_context = Arc::clone(&context);
        let app_reconcile_context = Arc::clone(&context);
        let app_owner_sid = owner_sid.clone();
        let service_pid = std::process::id();
        let result = runtime.block_on(async move {
            tokio::select! {
                result = pipe::serve(owner_sid, service_sid, listener, context, true) => result.context("named-pipe server stopped"),
                _ = enforcement::run_reconciler(reconcile_context) => Ok(()),
                _ = focuser_service::app_enforcement::run_app_enforcement_reconciler(
                    app_reconcile_context,
                    app_owner_sid,
                    service_pid,
                ) => Ok(()),
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
        status_handle: &service_control_handler::ServiceStatusHandle,
    ) -> anyhow::Result<(
        String,
        String,
        Runtime,
        NamedPipeServer,
        Arc<focuser_app::AppContext>,
    )> {
        report_start_pending(status_handle, 2)?;
        let options = parse_options(&arguments)?;
        let owner_sid = options
            .owner_sid
            .context("service configuration must include --owner-sid <SID>")?;
        let database_path = options
            .database_path
            .context("service configuration must include --database <PATH>")?;
        let hosts_path = options
            .hosts_path
            .context("service configuration must include --hosts-file <PATH>")?;
        report_start_pending(status_handle, 3)?;
        let service_sid = pipe::service_sid_string(SERVICE_NAME)
            .context("resolve FocusGuard service SID; install with a service SID enabled")?;
        if !pipe::current_process_has_enabled_sid(&service_sid)? {
            bail!(
                "service SID is not enabled in the SCM process token; enable SERVICE_SID_TYPE_UNRESTRICTED or SERVICE_SID_TYPE_RESTRICTED"
            );
        }
        report_start_pending(status_handle, 4)?;
        let context = Arc::new(enforcement::open_context(
            database_path,
            hosts_path,
            enforcement::flush_system_dns,
        )?);
        report_start_pending(status_handle, 5)?;
        enforcement::reconcile_once(&context)
            .map_err(|error| anyhow::anyhow!("initial policy reconciliation failed: {error}"))?;
        report_start_pending(status_handle, 6)?;
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        report_start_pending(status_handle, 7)?;
        let listener = runtime
            .block_on(pipe::prepare(&owner_sid, &service_sid))
            .context("prepare secured pipe listener and verify service SID access")?;
        Ok((owner_sid, service_sid, runtime, listener, context))
    }

    fn report_start_pending(
        status_handle: &service_control_handler::ServiceStatusHandle,
        checkpoint: u32,
    ) -> anyhow::Result<()> {
        let mut service_status = status(
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            ServiceExitCode::NO_ERROR,
        );
        service_status.checkpoint = checkpoint;
        service_status.wait_hint = Duration::from_secs(30);
        status_handle
            .set_service_status(service_status)
            .context("report service startup progress")
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
