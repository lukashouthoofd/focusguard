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
        fs::OpenOptions,
        io::Write,
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
        diagnostics_path: Option<PathBuf>,
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
            } else if arg == "--diagnostics-file" {
                let path = args.next().context("--diagnostics-file requires a path")?;
                if parsed
                    .diagnostics_path
                    .replace(PathBuf::from(path))
                    .is_some()
                {
                    bail!("--diagnostics-file may only be supplied once");
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

    /// `windows-service` passes the registered service name as argv[0]. The
    /// console entry point already strips its mode flag before parsing, so the
    /// SCM entry point removes this dispatcher-only argument first.
    fn strip_service_name_argument(arguments: Vec<OsString>) -> Vec<OsString> {
        if arguments
            .first()
            .and_then(|argument| argument.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case(SERVICE_NAME))
        {
            arguments.into_iter().skip(1).collect()
        } else {
            arguments
        }
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
        let process_arguments = service_process_arguments();
        let diagnostic_path = diagnostics_path_from_arguments(&process_arguments);
        let panic_diagnostic_path = diagnostic_path.clone();
        std::panic::set_hook(Box::new(move |panic| {
            record_diagnostic(
                panic_diagnostic_path.as_deref(),
                &format!("service panic: {panic}"),
            );
        }));
        record_diagnostic(
            diagnostic_path.as_deref(),
            "SCM dispatched service entry point",
        );
        if let Err(error) = run_service(arguments) {
            record_diagnostic(
                diagnostic_path.as_deref(),
                &format!("service startup/runtime failed: {error:#}"),
            );
            eprintln!("FocusGuard service failed: {error:#}");
        }
    }

    fn diagnostics_path_from_arguments(arguments: &[OsString]) -> Option<PathBuf> {
        let mut args = arguments.iter();
        while let Some(argument) = args.next() {
            if argument == "--diagnostics-file" {
                return args.next().map(PathBuf::from);
            }
        }
        None
    }

    /// ImagePath arguments belong to the service process command line. The
    /// ServiceMain argument vector contains only the service name and any
    /// arguments supplied to StartService by its caller.
    fn service_process_arguments() -> Vec<OsString> {
        std::env::args_os().skip(1).collect()
    }

    fn record_diagnostic(path: Option<&std::path::Path>, message: &str) {
        let Some(path) = path else { return };
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{message}");
        }
    }

    fn run_service(_dispatcher_arguments: Vec<OsString>) -> anyhow::Result<()> {
        // Keep the dispatcher vector out of configuration parsing: it carries
        // StartService arguments, not the process ImagePath arguments.
        let _service_specific_arguments = strip_service_name_argument(_dispatcher_arguments);
        let arguments = service_process_arguments();
        let diagnostic_path = diagnostics_path_from_arguments(&arguments);
        record_diagnostic(
            diagnostic_path.as_deref(),
            "registering SCM control handler",
        );
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

        record_diagnostic(diagnostic_path.as_deref(), "reporting startup checkpoint 1");
        report_start_pending(&status_handle, 1)?;
        record_diagnostic(
            diagnostic_path.as_deref(),
            "preparing service runtime and policy",
        );
        let (owner_sid, service_sid, runtime, listener, context) =
            match prepare_service(arguments, &status_handle) {
                Ok(prepared) => prepared,
                Err(error) => {
                    record_diagnostic(
                        diagnostic_path.as_deref(),
                        &format!("service preparation failed: {error:#}"),
                    );
                    let _ = status_handle.set_service_status(status(
                        ServiceState::Stopped,
                        ServiceControlAccept::empty(),
                        ServiceExitCode::Win32(1),
                    ));
                    return Err(error);
                }
            };
        record_diagnostic(diagnostic_path.as_deref(), "reporting running status");
        status_handle.set_service_status(status(
            ServiceState::Running,
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            ServiceExitCode::NO_ERROR,
        ))?;
        record_diagnostic(diagnostic_path.as_deref(), "service reported Running");

        let reconcile_context = Arc::clone(&context);
        let app_reconcile_context = Arc::clone(&context);
        let app_owner_sid = owner_sid.clone();
        let service_pid = std::process::id();
        let pipe_diagnostics = diagnostic_path.clone();
        let domain_diagnostics = diagnostic_path.clone();
        let app_diagnostics = diagnostic_path.clone();
        record_diagnostic(diagnostic_path.as_deref(), "entering async service loop");
        let result = runtime.block_on(async move {
            tokio::select! {
                result = async move {
                    record_diagnostic(pipe_diagnostics.as_deref(), "polling named-pipe server future");
                    pipe::serve(owner_sid, service_sid, listener, context, true).await
                } => result.context("named-pipe server stopped"),
                _ = async move {
                    record_diagnostic(domain_diagnostics.as_deref(), "polling domain reconciler future");
                    enforcement::run_reconciler(reconcile_context).await
                } => Ok(()),
                _ = async move {
                    record_diagnostic(app_diagnostics.as_deref(), "polling app reconciler future");
                    focuser_service::app_enforcement::run_app_enforcement_reconciler(
                        app_reconcile_context,
                        app_owner_sid,
                        service_pid,
                    ).await
                } => Ok(()),
                _ = shutdown_rx => Ok(()),
            }
        });
        record_diagnostic(
            diagnostic_path.as_deref(),
            &format!("async service loop returned: {result:?}"),
        );
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
        let diagnostic_path = diagnostics_path_from_arguments(&arguments);
        record_diagnostic(diagnostic_path.as_deref(), "reporting startup checkpoint 2");
        report_start_pending(status_handle, 2)?;
        record_diagnostic(diagnostic_path.as_deref(), "parsing service arguments");
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
        record_diagnostic(diagnostic_path.as_deref(), "validating service SID");
        report_start_pending(status_handle, 3)?;
        let service_sid = pipe::service_sid_string(SERVICE_NAME)
            .context("resolve FocusGuard service SID; install with a service SID enabled")?;
        if !pipe::current_process_has_enabled_sid(&service_sid)? {
            bail!(
                "service SID is not enabled in the SCM process token; enable SERVICE_SID_TYPE_UNRESTRICTED or SERVICE_SID_TYPE_RESTRICTED"
            );
        }
        record_diagnostic(diagnostic_path.as_deref(), "opening policy database");
        report_start_pending(status_handle, 4)?;
        let context = Arc::new(enforcement::open_context(
            database_path,
            hosts_path,
            enforcement::flush_system_dns,
        )?);
        record_diagnostic(
            diagnostic_path.as_deref(),
            "reconciling initial domain policy",
        );
        report_start_pending(status_handle, 5)?;
        enforcement::reconcile_once(&context)
            .map_err(|error| anyhow::anyhow!("initial policy reconciliation failed: {error}"))?;
        record_diagnostic(diagnostic_path.as_deref(), "building async runtime");
        report_start_pending(status_handle, 6)?;
        let runtime = Builder::new_multi_thread().enable_all().build()?;
        record_diagnostic(diagnostic_path.as_deref(), "creating secured named pipe");
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

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn strips_registered_service_name_before_parsing_configuration() {
            let arguments = vec![
                OsString::from("focusguardservice"),
                OsString::from("--owner-sid"),
                OsString::from("S-1-5-21-1"),
            ];

            assert_eq!(
                strip_service_name_argument(arguments),
                [OsString::from("--owner-sid"), OsString::from("S-1-5-21-1")]
            );
        }

        #[test]
        fn preserves_arguments_when_dispatcher_name_is_absent() {
            let arguments = vec![OsString::from("--owner-sid"), OsString::from("S-1-5-21-1")];

            assert_eq!(strip_service_name_argument(arguments.clone()), arguments);
        }

        #[test]
        fn reads_diagnostics_path_from_executable_arguments() {
            let arguments = vec![
                OsString::from("--diagnostics-file"),
                OsString::from("C:\\temp\\service.log"),
            ];

            assert_eq!(
                diagnostics_path_from_arguments(&arguments),
                Some(PathBuf::from("C:\\temp\\service.log"))
            );
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
