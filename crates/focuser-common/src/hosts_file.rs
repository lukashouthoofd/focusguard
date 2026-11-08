//! Safe replacement of FocusGuard's owned section in an OS hosts file.
//!
//! The path is injected so callers and tests can use the same implementation
//! without touching the machine-wide hosts file. Only one balanced FocusGuard
//! section is accepted; malformed ownership markers fail closed.

use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

#[cfg(windows)]
use windows::{
    Win32::Storage::FileSystem::{REPLACE_FILE_FLAGS, ReplaceFileW},
    core::PCWSTR,
};

const HOSTS_BEGIN: &str = "# ──── BEGIN FOCUSGUARD BLOCK ────";
const HOSTS_END: &str = "# ──── END FOCUSGUARD BLOCK ────";
const HOSTS_NOTE: &str = "# Added by FocusGuard to block sites. If FocusGuard is gone, delete this block from BEGIN to END.";

static HOSTS_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostsSyncOutcome {
    Unchanged,
    Updated,
}

#[derive(Debug, thiserror::Error)]
pub enum HostsFileError {
    #[error("cannot read hosts file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid FocusGuard section in hosts file: {0}")]
    InvalidSection(&'static str),

    #[error("invalid hosts-file domain: {0:?}")]
    InvalidDomain(String),

    #[error("cannot prepare replacement for hosts file {path}: {source}")]
    Prepare {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("hosts file {0} changed while FocusGuard was preparing an update")]
    Conflict(PathBuf),

    #[error("cannot replace hosts file {path}: {source}")]
    Replace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Update or remove FocusGuard's section in `path`.
///
/// Writes a same-directory temporary file, flushes it, checks that the
/// original has not changed during preparation, then replaces the file. On
/// Windows, `ReplaceFileW` preserves the original file's ACL and attributes.
pub fn sync_path(
    path: impl AsRef<Path>,
    domains: &[String],
) -> Result<HostsSyncOutcome, HostsFileError> {
    let path = path.as_ref();
    let lock = HOSTS_WRITE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().map_err(|_| HostsFileError::Prepare {
        path: path.to_path_buf(),
        source: io::Error::other("hosts-file writer lock poisoned"),
    })?;

    let original = fs::read(path).map_err(|source| HostsFileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let original_text = std::str::from_utf8(&original).map_err(|error| HostsFileError::Read {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidData, error),
    })?;
    let updated_text = replace_section(original_text, domains)?;
    let updated = updated_text.as_bytes();
    if original == updated {
        return Ok(HostsSyncOutcome::Unchanged);
    }

    #[cfg(unix)]
    let metadata = fs::metadata(path).map_err(|source| HostsFileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let (temporary_path, mut temporary_file) = create_temporary_file(path)?;
    let result = (|| {
        temporary_file
            .write_all(updated)
            .and_then(|()| temporary_file.sync_all())
            .map_err(|source| HostsFileError::Prepare {
                path: path.to_path_buf(),
                source,
            })?;
        drop(temporary_file);

        #[cfg(unix)]
        fs::set_permissions(&temporary_path, metadata.permissions()).map_err(|source| {
            HostsFileError::Prepare {
                path: path.to_path_buf(),
                source,
            }
        })?;

        // Do not silently overwrite a hosts-file edit made while the new
        // content was being prepared. This is a best-effort conflict check;
        // external editors do not participate in FocusGuard's writer lock.
        let current = fs::read(path).map_err(|source| HostsFileError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if current != original {
            return Err(HostsFileError::Conflict(path.to_path_buf()));
        }

        replace_file(path, &temporary_path).map_err(|source| HostsFileError::Replace {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result?;
    Ok(HostsSyncOutcome::Updated)
}

/// Transform the hosts-file text while preserving unrelated lines byte for
/// byte, including CRLF and a missing final newline.
pub fn replace_section(content: &str, domains: &[String]) -> Result<String, HostsFileError> {
    let lines: Vec<&str> = content.split_inclusive('\n').collect();
    let mut begins = Vec::new();
    let mut ends = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let line = line.trim_end_matches(['\r', '\n']).trim();
        if line == HOSTS_BEGIN {
            begins.push(index);
        } else if line == HOSTS_END {
            ends.push(index);
        }
    }

    let (prefix_end, suffix_start) = match (begins.as_slice(), ends.as_slice()) {
        ([], []) => (lines.len(), lines.len()),
        ([begin], [end]) if begin < end => (*begin, end + 1),
        ([], [_]) => {
            return Err(HostsFileError::InvalidSection(
                "end marker has no begin marker",
            ));
        }
        ([_], []) => {
            return Err(HostsFileError::InvalidSection(
                "begin marker has no end marker",
            ));
        }
        ([_, ..], [_, ..]) => {
            return Err(HostsFileError::InvalidSection(
                "multiple begin or end markers are ambiguous",
            ));
        }
        _ => return Err(HostsFileError::InvalidSection("markers are out of order")),
    };

    let prefix = lines[..prefix_end].concat();
    let suffix = lines[suffix_start..].concat();
    let mut result = String::with_capacity(content.len() + domains.len() * 40);
    result.push_str(&prefix);

    if domains.is_empty() {
        result.push_str(&suffix);
        return Ok(result);
    }

    let mut unique_domains = BTreeSet::new();
    for domain in domains {
        if !valid_hosts_name(domain) {
            return Err(HostsFileError::InvalidDomain(domain.clone()));
        }
        unique_domains.insert(domain.to_ascii_lowercase());
    }

    if unique_domains.is_empty() {
        result.push_str(&suffix);
        return Ok(result);
    }

    let newline = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    if !result.is_empty() && !result.ends_with('\n') {
        result.push_str(newline);
    }
    result.push_str(HOSTS_BEGIN);
    result.push_str(newline);
    result.push_str(HOSTS_NOTE);
    result.push_str(newline);
    for domain in unique_domains {
        result.push_str(&format!("127.0.0.1 {domain}{newline}"));
        result.push_str(&format!("::1 {domain}{newline}"));
    }
    result.push_str(HOSTS_END);
    result.push_str(newline);
    result.push_str(&suffix);
    Ok(result)
}

fn valid_hosts_name(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 || !domain.is_ascii() {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn create_temporary_file(path: &Path) -> Result<(PathBuf, File), HostsFileError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| HostsFileError::Prepare {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "hosts path has no file name"),
        })?
        .to_string_lossy();

    for _ in 0..16 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary_path = parent.join(format!(
            ".{file_name}.focusguard-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(HostsFileError::Prepare {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
    Err(HostsFileError::Prepare {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique temporary file",
        ),
    })
}

#[cfg(windows)]
fn replace_file(path: &Path, temporary_path: &Path) -> io::Result<()> {
    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let path = wide(path);
    let temporary_path = wide(temporary_path);
    unsafe {
        ReplaceFileW(
            PCWSTR(path.as_ptr()),
            PCWSTR(temporary_path.as_ptr()),
            PCWSTR::null(),
            REPLACE_FILE_FLAGS(0),
            None,
            None,
        )
    }
    .map_err(io::Error::other)
}

#[cfg(not(windows))]
fn replace_file(path: &Path, temporary_path: &Path) -> io::Result<()> {
    fs::rename(temporary_path, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_is_idempotent_and_remove_preserves_original_lines() {
        let original = "# custom comment\r\n127.0.0.1 localhost\r\n";
        let blocked = replace_section(original, &["reddit.com".into()]).unwrap();
        assert!(blocked.starts_with(original));
        assert!(blocked.contains("127.0.0.1 reddit.com\r\n"));
        assert!(blocked.contains("::1 reddit.com\r\n"));
        assert_eq!(
            replace_section(&blocked, &["reddit.com".into()]).unwrap(),
            blocked
        );
        assert_eq!(replace_section(&blocked, &[]).unwrap(), original);
    }

    #[test]
    fn preserves_crlf_and_unterminated_unrelated_suffix() {
        let original = format!("# before\r\n{HOSTS_BEGIN}\r\nold block\r\n{HOSTS_END}\r\n# after");
        let updated = replace_section(&original, &["reddit.com".into()]).unwrap();
        assert!(updated.starts_with("# before\r\n"));
        assert!(updated.ends_with("# after"));
        assert!(updated.contains("127.0.0.1 reddit.com\r\n"));
        assert_eq!(
            replace_section(&updated, &[]).unwrap(),
            "# before\r\n# after"
        );
        assert!(
            !updated.contains("\n")
                || updated.matches("\n").count() == updated.matches("\r\n").count()
        );
    }

    #[test]
    fn malformed_or_duplicate_markers_fail_without_a_rendered_result() {
        for content in [
            format!("safe\n{HOSTS_BEGIN}\nkeep this\n"),
            format!("safe\n{HOSTS_END}\nkeep this\n"),
            format!("{HOSTS_END}\n{HOSTS_BEGIN}\n"),
            format!("{HOSTS_BEGIN}\na\n{HOSTS_END}\n{HOSTS_BEGIN}\nb\n{HOSTS_END}\n"),
        ] {
            assert!(matches!(
                replace_section(&content, &["reddit.com".into()]),
                Err(HostsFileError::InvalidSection(_))
            ));
        }
    }

    #[test]
    fn domains_cannot_inject_hosts_lines() {
        for invalid in [
            "",
            "bad domain",
            "bad.example\n0.0.0.0 other",
            "*.example.com",
            "-bad.example",
            "bad-.example",
        ] {
            assert!(
                matches!(
                    replace_section("127.0.0.1 localhost\n", &[invalid.into()]),
                    Err(HostsFileError::InvalidDomain(_))
                ),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn file_sync_replaces_atomically_and_preserves_unrelated_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts");
        let original = b"# user line\r\n127.0.0.1 localhost\r\n";
        fs::write(&path, original).unwrap();

        assert_eq!(
            sync_path(&path, &["reddit.com".into()]).unwrap(),
            HostsSyncOutcome::Updated
        );
        let blocked = fs::read(&path).unwrap();
        assert!(blocked.starts_with(original));
        assert_eq!(
            sync_path(&path, &["reddit.com".into()]).unwrap(),
            HostsSyncOutcome::Unchanged
        );
        assert_eq!(fs::read(&path).unwrap(), blocked);
        assert_eq!(sync_path(&path, &[]).unwrap(), HostsSyncOutcome::Updated);
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    fn malformed_file_is_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts");
        let original = format!("unrelated\n{HOSTS_BEGIN}\nimportant unrelated text\n");
        fs::write(&path, &original).unwrap();

        assert!(sync_path(&path, &["reddit.com".into()]).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(fs::read_dir(directory.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }
}
