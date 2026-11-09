#[cfg(windows)]
mod pipe;

#[cfg(windows)]
use focuser_service::enforcement;

#[cfg(target_os = "macos")]
mod macos_service_host {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        path::Path,
        sync::Arc,
        time::Duration,
    };

    use anyhow::{Context, bail};
    use focuser_common::ipc::{
        FOCUSGUARD_SERVICE_DATABASE_PATH, FOCUSGUARD_SERVICE_OWNER_UID_PATH,
        read_macos_service_owner_uid,
    };
    use focuser_service::{enforcement, unix_socket};
    use rusqlite::{Connection, OpenFlags, backup::Backup};

    pub async fn run() -> anyhow::Result<()> {
        let arguments: Vec<_> = std::env::args_os().skip(1).collect();
        if arguments.len() != 1 || arguments[0] != "--macos-daemon" {
            bail!("the macOS helper must be launched with --macos-daemon");
        }
        if unsafe { libc::geteuid() } != 0 {
            bail!("the macOS helper must be launched by launchd as root");
        }
        unsafe { libc::umask(0o077) };

        ensure_support_directories()?;
        let owner_uid = owner_uid_for_service().await?;
        migrate_user_database(owner_uid)?;
        let (listener, _socket_guard) = unix_socket::bind_macos_listener(owner_uid)
            .await
            .context("prepare the authenticated macOS service socket")?;
        let context = Arc::new(enforcement::open_context(
            FOCUSGUARD_SERVICE_DATABASE_PATH,
            "/etc/hosts",
            enforcement::flush_system_dns,
        )?);
        if let Err(error) = enforcement::reconcile_once(&context) {
            eprintln!("initial FocusGuard policy reconciliation failed: {error}");
        }

        let reconcile_context = Arc::clone(&context);
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("listen for launchd shutdown")?;
        tokio::select! {
            result = unix_socket::serve(listener, owner_uid, Arc::clone(&context), true) => {
                result.context("macOS service socket stopped")
            }
            _ = enforcement::run_reconciler(reconcile_context) => Ok(()),
            Some(()) = terminate.recv() => {
                context.sync_hosts_with(&[]).map_err(anyhow::Error::msg)
                    .context("restore hosts file before the service exits")?;
                Ok(())
            }
            result = tokio::signal::ctrl_c() => result.context("wait for shutdown signal"),
        }
    }

    async fn owner_uid_for_service() -> anyhow::Result<u32> {
        loop {
            if let Some(owner_uid) = read_macos_service_owner_uid()? {
                return Ok(owner_uid);
            }

            let console = fs::metadata("/dev/console").context("read the active console user")?;
            let owner_uid = console.uid();
            // /dev/console can briefly belong to setup/loginwindow accounts
            // that do not have a normal user home. Do not persist one of
            // those UIDs: launchd will keep restarting us, and a marker would
            // otherwise pin every restart to that ineligible account.
            if owner_uid != 0 && user_home(owner_uid).is_ok() {
                write_owner_marker(owner_uid)?;
                return Ok(owner_uid);
            }

            // launchd may start this daemon before the first graphical login.
            // Wait without creating an unowned socket or opening a root policy DB.
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    fn ensure_support_directories() -> anyhow::Result<()> {
        // /Library/Application Support is shared with the whole system. Only
        // inspect it; never change permissions on an administrator-managed
        // directory. New directories below it are ours to create with the
        // traversal bits required by the per-user socket client.
        validate_support_directory(Path::new("/Library/Application Support"), 0)?;
        ensure_product_directory(Path::new("/Library/Application Support/PublikHQ"), 0)?;
        ensure_product_directory(
            Path::new("/Library/Application Support/PublikHQ/FocusGuard"),
            0,
        )?;
        Ok(())
    }

    fn validate_support_directory(path: &Path, owner_uid: u32) -> anyhow::Result<()> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("inspect support directory {}", path.display()))?;
        if !metadata.file_type().is_dir()
            || metadata.uid() != owner_uid
            || metadata.mode() & 0o022 != 0
            || metadata.mode() & 0o001 == 0
        {
            bail!(
                "{} must be an owner-controlled directory, protected from group/world writes, and traversable by the service client",
                path.display()
            );
        }
        Ok(())
    }

    fn ensure_product_directory(path: &Path, owner_uid: u32) -> anyhow::Result<()> {
        match fs::create_dir(path) {
            Ok(()) => fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                .with_context(|| format!("set permissions on {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("create {}", path.display()));
            }
        }
        validate_support_directory(path, owner_uid)
    }

    fn write_owner_marker(owner_uid: u32) -> anyhow::Result<()> {
        let marker = Path::new(FOCUSGUARD_SERVICE_OWNER_UID_PATH);
        let temporary = marker.with_extension(format!("tmp.{}", unsafe { libc::getpid() }));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&temporary)
            .context("create the temporary service owner marker")?;
        writeln!(file, "{owner_uid}").context("write the service owner marker")?;
        file.sync_all().context("sync the service owner marker")?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644))?;
        fs::rename(&temporary, marker).context("publish the service owner marker")?;
        Ok(())
    }

    fn migrate_user_database(owner_uid: u32) -> anyhow::Result<()> {
        let home = user_home(owner_uid)?;
        let source = home.join("Library/Application Support/com.publikhq.FocusGuard/focusguard.db");
        let destination = Path::new(FOCUSGUARD_SERVICE_DATABASE_PATH);

        if validate_service_database(destination, 0)? {
            for suffix in ["-wal", "-shm"] {
                let sidecar = sqlite_sidecar_path(destination, suffix);
                validate_service_database(&sidecar, 0)
                    .with_context(|| format!("validate SQLite sidecar {}", sidecar.display()))?;
            }
            return Ok(());
        }
        for suffix in ["-wal", "-shm"] {
            let sidecar = sqlite_sidecar_path(destination, suffix);
            if validate_service_database(&sidecar, 0)? {
                bail!(
                    "a SQLite sidecar exists without the system policy database: {}",
                    sidecar.display()
                );
            }
        }

        let source_metadata = match fs::symlink_metadata(&source) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspect the user's policy database"),
        };
        if !source_metadata.file_type().is_file() || source_metadata.uid() != owner_uid {
            bail!("the user's policy database must be a regular file owned by the console user");
        }

        let temporary =
            destination.with_extension(format!("migrate.{}", unsafe { libc::getpid() }));
        let source_db = Connection::open_with_flags(&source, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .context("open the user's policy database read-only")?;
        let mut destination_db =
            Connection::open(&temporary).context("create a temporary system policy database")?;
        {
            let backup = Backup::new(&source_db, &mut destination_db)
                .context("prepare the user's policy database migration")?;
            backup
                .run_to_completion(128, Duration::from_millis(25), None)
                .context("copy the user's policy database")?;
        }
        drop(destination_db);
        fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temporary, destination).context("publish the system policy database")?;
        Ok(())
    }

    fn validate_service_database(path: &Path, owner_uid: u32) -> anyhow::Result<bool> {
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
        };
        if !metadata.file_type().is_file()
            || metadata.uid() != owner_uid
            || metadata.mode() & 0o077 != 0
            || metadata.mode() & 0o600 != 0o600
        {
            bail!(
                "{} must be a private regular file owned by UID {owner_uid}",
                path.display()
            );
        }
        Ok(true)
    }

    fn sqlite_sidecar_path(database: &Path, suffix: &str) -> std::path::PathBuf {
        let name = database
            .file_name()
            .expect("the system database path has a file name")
            .to_string_lossy();
        database.with_file_name(format!("{name}{suffix}"))
    }

    fn user_home(uid: u32) -> anyhow::Result<std::path::PathBuf> {
        use std::{ffi::CStr, mem::MaybeUninit, ptr};

        let suggested_size = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        let buffer_size = if suggested_size > 0 {
            suggested_size as usize
        } else {
            16 * 1024
        };
        let mut buffer = vec![0u8; buffer_size];
        let mut record = MaybeUninit::<libc::passwd>::zeroed();
        let mut result = ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                uid as libc::uid_t,
                record.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status != 0 {
            return Err(std::io::Error::from_raw_os_error(status))
                .context("look up the console user's home directory");
        }
        if result.is_null() {
            bail!("no home directory exists for console UID {uid}");
        }
        let record = unsafe { record.assume_init() };
        let home = unsafe { CStr::from_ptr(record.pw_dir) }.to_string_lossy();
        if home.is_empty() || !home.starts_with("/Users/") {
            bail!("console UID {uid} does not have a user home under /Users");
        }
        Ok(std::path::PathBuf::from(home.as_ref()))
    }

    #[cfg(test)]
    mod tests {
        use std::{
            fs,
            os::unix::{fs::MetadataExt, fs::PermissionsExt},
        };

        use tempfile::tempdir;

        use super::{
            ensure_product_directory, sqlite_sidecar_path, validate_service_database,
            validate_support_directory,
        };

        #[test]
        fn shared_support_validation_does_not_change_existing_permissions() {
            let directory = tempdir().unwrap();
            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o711)).unwrap();

            assert!(
                validate_support_directory(directory.path(), unsafe { libc::geteuid() }).is_ok()
            );
            assert_eq!(
                fs::symlink_metadata(directory.path()).unwrap().mode() & 0o777,
                0o711
            );

            fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
            assert!(
                validate_support_directory(directory.path(), unsafe { libc::geteuid() }).is_err()
            );
            assert_eq!(
                fs::symlink_metadata(directory.path()).unwrap().mode() & 0o777,
                0o700,
                "validation must not relax an existing directory"
            );
        }

        #[test]
        fn product_directory_is_created_with_client_traversal_permissions() {
            let parent = tempdir().unwrap();
            let product = parent.path().join("FocusGuard");

            ensure_product_directory(&product, unsafe { libc::geteuid() }).unwrap();

            assert_eq!(fs::symlink_metadata(product).unwrap().mode() & 0o777, 0o755);
        }

        #[test]
        fn service_database_and_sidecars_must_be_private_writable_files() {
            let directory = tempdir().unwrap();
            let database = directory.path().join("focusguard.db");
            fs::write(&database, b"test").unwrap();
            let owner_uid = unsafe { libc::geteuid() };
            fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(validate_service_database(&database, owner_uid).unwrap());

            fs::set_permissions(&database, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(validate_service_database(&database, owner_uid).is_err());

            let wal = sqlite_sidecar_path(&database, "-wal");
            fs::write(&wal, b"test").unwrap();
            fs::set_permissions(&wal, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(validate_service_database(&wal, owner_uid).unwrap());
            fs::set_permissions(&wal, fs::Permissions::from_mode(0o400)).unwrap();
            assert!(validate_service_database(&wal, owner_uid).is_err());
        }
    }
}

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
        record_diagnostic(diagnostic_path.as_deref(), "entering async service loop");
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

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    macos_service_host::run().await
}

#[cfg(not(any(windows, target_os = "macos")))]
fn main() {
    eprintln!("focusguard-service is available only on Windows and macOS");
}
