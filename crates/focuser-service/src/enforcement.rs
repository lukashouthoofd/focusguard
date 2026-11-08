//! Persistent, exact-domain Windows hosts-file enforcement owned by the
//! service process. Callers supply both paths so tests can use temporary files.

use std::{
    fs::OpenOptions,
    io,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use focuser_app::{AppContext, SystemSync};
use focuser_core::{BlockEngine, Database};

pub struct HostsFileSync {
    path: PathBuf,
    flush_dns: fn() -> io::Result<()>,
    dns_flush_pending: Mutex<bool>,
}

impl HostsFileSync {
    pub fn new(path: impl Into<PathBuf>, flush_dns: fn() -> io::Result<()>) -> Self {
        Self {
            path: path.into(),
            flush_dns,
            // Startup reconciliation must refresh DNS even if the previous
            // process had already written the same hosts entries.
            dns_flush_pending: Mutex::new(true),
        }
    }
}

impl SystemSync for HostsFileSync {
    fn sync_hosts(&self, domains: &[String]) -> Result<(), String> {
        let outcome = focuser_common::hosts_file::sync_path(&self.path, domains)
            .map_err(|error| error.to_string())?;
        let mut dns_flush_pending = self
            .dns_flush_pending
            .lock()
            .map_err(|_| "DNS flush state lock poisoned".to_string())?;
        if outcome == focuser_common::hosts_file::HostsSyncOutcome::Updated {
            *dns_flush_pending = true;
        }
        if !*dns_flush_pending {
            return Ok(());
        }

        (self.flush_dns)().map_err(|error| {
            format!("hosts policy is applied, but DNS cache flush failed: {error}")
        })?;
        *dns_flush_pending = false;
        Ok(())
    }

    fn hosts_writable(&self) -> bool {
        OpenOptions::new().append(true).open(&self.path).is_ok()
    }
}

pub fn open_context(
    database_path: impl AsRef<Path>,
    hosts_path: impl Into<PathBuf>,
    flush_dns: fn() -> io::Result<()>,
) -> io::Result<AppContext> {
    let database = Database::open(database_path).map_err(other_io)?;
    let engine = BlockEngine::new(database).map_err(other_io)?;
    Ok(AppContext::new(
        engine,
        Arc::new(HostsFileSync::new(hosts_path, flush_dns)),
    ))
}

/// Reload durable schedules and push their current exact-domain set to the
/// injected hosts-file path. A failed apply remains observable through the
/// context's blocking-health status.
pub fn reconcile_once(context: &AppContext) -> Result<(), String> {
    let mut engine = context
        .engine
        .lock()
        .map_err(|_| "engine lock poisoned".to_string())?;
    engine.refresh().map_err(|error| error.to_string())?;
    context.sync_hosts(&engine)
}

pub async fn run_reconciler(context: Arc<AppContext>) {
    let mut interval = tokio::time::interval(Duration::from_secs(2));
    loop {
        interval.tick().await;
        if let Err(error) = reconcile_once(&context) {
            tracing::warn!(%error, "service policy reconciliation failed");
        }
    }
}

fn other_io(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

pub fn flush_system_dns() -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStringExt, process::CommandExt};
        use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

        let mut buffer = [0u16; 32768];
        let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length as usize >= buffer.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows system directory path is too long",
            ));
        }
        let mut executable =
            PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
        executable.push("ipconfig.exe");
        let output = Command::new(executable)
            .args(["/flushdns"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "ipconfig.exe /flushdns exited with {}",
                output.status
            )))
        }
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focuser_app::{Command, execute::execute};
    use focuser_common::types::WebsiteMatchType;
    use std::fs;

    fn no_flush() -> io::Result<()> {
        Ok(())
    }

    #[test]
    fn active_domain_rules_survive_context_restart_and_are_reapplied() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("service.db");
        let hosts = directory.path().join("hosts");
        let original = "# machine comment\r\n127.0.0.1 localhost\r\n";
        fs::write(&hosts, original).unwrap();

        {
            let context = open_context(&database, &hosts, no_flush).unwrap();
            let list = execute(
                &context,
                Command::CreateBlockList {
                    name: "Work".into(),
                },
            )
            .unwrap()
            .as_block_list()
            .unwrap()
            .clone();
            execute(
                &context,
                Command::ToggleBlockList {
                    id: list.id,
                    enabled: true,
                },
            )
            .unwrap();
            execute(
                &context,
                Command::AddWebsiteRule {
                    list_id: list.id,
                    rule: WebsiteMatchType::Domain("reddit.com".into()),
                },
            )
            .unwrap();

            let applied = fs::read_to_string(&hosts).unwrap();
            assert!(applied.starts_with(original));
            assert!(applied.contains("127.0.0.1 www.reddit.com\r\n"));
            assert!(applied.contains("127.0.0.1 reddit.com\r\n"));
            assert!(applied.contains("::1 www.reddit.com\r\n"));
        }

        let context = open_context(&database, &hosts, no_flush).unwrap();
        reconcile_once(&context).unwrap();
        let reapplied = fs::read_to_string(&hosts).unwrap();
        assert!(reapplied.contains("127.0.0.1 www.reddit.com\r\n"));
        assert!(reapplied.contains("127.0.0.1 reddit.com\r\n"));
        assert_eq!(
            focuser_common::hosts_file::sync_path(
                &hosts,
                &context.engine.lock().unwrap().collect_blocked_domains()
            )
            .unwrap(),
            focuser_common::hosts_file::HostsSyncOutcome::Unchanged
        );
    }

    #[test]
    fn policy_write_failure_is_reported_in_health() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("service.db");
        let missing_hosts = directory.path().join("missing").join("hosts");
        let context = open_context(&database, &missing_hosts, no_flush).unwrap();

        assert!(reconcile_once(&context).is_err());
        assert!(context.hosts_sync_error().is_some());
        assert!(!context.hosts_writable());
    }

    #[test]
    fn flush_failure_is_reported_after_hosts_content_is_committed() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static FLUSH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

        fn fail_on_second_flush() -> io::Result<()> {
            if FLUSH_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err(io::Error::other("fixture flush failure"))
            }
        }

        FLUSH_ATTEMPTS.store(0, Ordering::SeqCst);
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("service.db");
        let hosts = directory.path().join("hosts");
        fs::write(&hosts, "127.0.0.1 localhost\n").unwrap();
        let context = open_context(&database, &hosts, fail_on_second_flush).unwrap();
        let list = execute(
            &context,
            Command::CreateBlockList {
                name: "Work".into(),
            },
        )
        .unwrap()
        .as_block_list()
        .unwrap()
        .clone();
        execute(
            &context,
            Command::ToggleBlockList {
                id: list.id,
                enabled: true,
            },
        )
        .unwrap();
        let result = execute(
            &context,
            Command::AddWebsiteRule {
                list_id: list.id,
                rule: WebsiteMatchType::Domain("reddit.com".into()),
            },
        );
        assert!(matches!(
            result,
            Err(focuser_app::CommandError::Enforcement(_))
        ));
        assert!(fs::read_to_string(&hosts).unwrap().contains("reddit.com"));
    }

    #[test]
    fn a_failed_dns_flush_is_retried_when_hosts_content_is_unchanged() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        static FLUSH_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);

        fn fail_once() -> io::Result<()> {
            if FLUSH_ATTEMPTS.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(io::Error::other("fixture first flush failure"))
            } else {
                Ok(())
            }
        }

        FLUSH_ATTEMPTS.store(0, Ordering::SeqCst);
        let directory = tempfile::tempdir().unwrap();
        let hosts = directory.path().join("hosts");
        fs::write(&hosts, "127.0.0.1 localhost\n").unwrap();
        let sync = HostsFileSync::new(&hosts, fail_once);
        let domains = vec!["reddit.com".into()];

        assert!(sync.sync_hosts(&domains).is_err());
        assert!(fs::read_to_string(&hosts).unwrap().contains("reddit.com"));
        sync.sync_hosts(&domains).unwrap();
        sync.sync_hosts(&domains).unwrap();
        assert_eq!(FLUSH_ATTEMPTS.load(Ordering::SeqCst), 2);
    }
}
