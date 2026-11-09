//! IPC message types for communication between CLI/GUI and the service.

use serde::{Deserialize, Serialize};

use crate::browser::BrowserStatusInfo;
use crate::extension::{BlockingCapabilities, ExtensionEvent, ExtensionRuleSet};
use crate::types::{BlockList, EntityId, UsageStat};

/// Versioned local named-pipe endpoint shared by the Windows client and service.
pub const FOCUSGUARD_SERVICE_PIPE_NAME: &str = r"\\.\pipe\PublikHQ.FocusGuard.Service.v1";

/// macOS local socket endpoint. The system service creates this socket for its
/// configured owner user inside a root-owned application-support directory.
#[cfg(target_os = "macos")]
pub const FOCUSGUARD_SERVICE_SOCKET_PATH: &str =
    "/Library/Application Support/PublikHQ/FocusGuard/service.sock";

/// Root-owned marker recording the console user that first activated the
/// macOS LaunchDaemon. Desktop clients can read it but cannot replace it.
#[cfg(target_os = "macos")]
pub const FOCUSGUARD_SERVICE_OWNER_UID_PATH: &str =
    "/Library/Application Support/PublikHQ/FocusGuard/service-owner-uid";

/// Fixed policy database owned by the macOS system service.
#[cfg(target_os = "macos")]
pub const FOCUSGUARD_SERVICE_DATABASE_PATH: &str =
    "/Library/Application Support/PublikHQ/FocusGuard/focusguard.db";

/// Read the owner UID from the root-owned macOS service marker.
///
/// The marker is readable by desktop clients but cannot be replaced or changed
/// by them. A missing marker means the privileged service has not been
/// registered; malformed or writable markers fail closed.
#[cfg(target_os = "macos")]
pub fn read_macos_service_owner_uid() -> std::io::Result<Option<u32>> {
    use std::{fs, os::unix::fs::MetadataExt, path::Path};

    let path = Path::new(FOCUSGUARD_SERVICE_OWNER_UID_PATH);
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > 16
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "macOS service owner marker is not a protected root-owned file",
        ));
    }

    let value = fs::read_to_string(path)?;
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "macOS service owner marker must contain one numeric UID",
        ));
    }
    let uid = value
        .parse::<u32>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if uid == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "macOS service owner UID must not be root",
        ));
    }
    Ok(Some(uid))
}

/// Request from client (CLI/GUI) to service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcRequest {
    // ─── Block list management ──────────────────────────────
    /// Create a new block list.
    CreateBlockList(BlockList),
    /// Update an existing block list.
    UpdateBlockList(BlockList),
    /// Delete a block list by ID.
    DeleteBlockList(EntityId),
    /// Get a block list by ID.
    GetBlockList(EntityId),
    /// List all block lists.
    ListBlockLists,
    /// Enable or disable a block list.
    SetBlockListEnabled {
        id: EntityId,
        enabled: bool,
    },

    // ─── Blocking control ───────────────────────────────────
    /// Start a block with an optional timer (minutes).
    StartBlock {
        block_list_id: EntityId,
        duration_minutes: Option<u32>,
    },
    /// Stop a block (only works if no lock prevents it).
    StopBlock {
        block_list_id: EntityId,
    },

    // ─── Status ─────────────────────────────────────────────
    /// Get current blocking status.
    GetStatus,
    /// Check if a specific domain is blocked.
    CheckDomain(String),
    /// Check if a specific app is blocked.
    CheckApp(String),

    // ─── Statistics ─────────────────────────────────────────
    /// Get usage statistics for a date range.
    GetStats {
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    },
    /// Get blocked attempt count.
    GetBlockedAttempts,

    // ─── Settings ────────────────────────────────────────────
    /// Get a setting value by key.
    GetSetting(String),
    /// Set a setting value.
    SetSetting {
        key: String,
        value: String,
    },

    // ─── Extension ──────────────────────────────────���────────
    /// Get the compiled rule set for the browser extension.
    GetExtensionRules,
    /// Report an event from the browser extension.
    ExtensionEvent(ExtensionEvent),
    /// Get current blocking capabilities (what's available).
    GetCapabilities,
    /// Get status of all detected browsers and their extension state.
    GetBrowserStatus,

    // ─── Protection ──────────────────────────────────────────
    EnableProtection {
        block_list_id: EntityId,
        duration_minutes: u32,
        prevent_uninstall: bool,
        prevent_service_stop: bool,
        prevent_modification: bool,
    },
    GetProtectionStatus,

    // ─── Service control ────────────────────────────────────
    /// Ping — check if service is alive.
    Ping,
    /// Graceful shutdown.
    Shutdown,
}

/// Response from service to client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IpcResponse {
    /// Operation succeeded with no data.
    Ok,
    /// Operation failed.
    Error(String),
    /// Single block list.
    BlockList(BlockList),
    /// Multiple block lists.
    BlockLists(Vec<BlockList>),
    /// Current status of the service.
    Status(ServiceStatus),
    /// Domain check result.
    DomainBlocked(bool),
    /// App check result.
    AppBlocked(bool),
    /// Usage statistics.
    Stats(Vec<UsageStat>),
    /// Blocked attempt count.
    BlockedAttempts(u64),
    /// Setting value (None if not set).
    Setting(Option<String>),
    /// Compiled rules for the browser extension.
    ExtensionRules(ExtensionRuleSet),
    /// Current blocking capabilities.
    Capabilities(BlockingCapabilities),
    /// Browser detection and extension status.
    BrowserStatus(Vec<BrowserStatusInfo>),
    ProtectionStatus(Vec<ProtectionInfo>),
    /// Pong response.
    Pong,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectionInfo {
    pub block_list_id: EntityId,
    pub block_list_name: String,
    pub prevent_uninstall: bool,
    pub prevent_service_stop: bool,
    pub prevent_modification: bool,
    /// `None` while the protection runs until unlocked.
    pub remaining_seconds: Option<u64>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Overall service status.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub running: bool,
    pub active_blocks: Vec<ActiveBlockInfo>,
    pub total_blocked_today: u64,
    pub uptime_seconds: u64,
}

/// Info about a currently active block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveBlockInfo {
    pub block_list_id: EntityId,
    pub block_list_name: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub blocked_websites: u32,
    pub blocked_apps: u32,
}
