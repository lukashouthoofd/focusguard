//! Bounded wire framing for the future Windows service boundary.
//!
//! This module only defines and validates messages. It does not create a pipe,
//! authenticate a peer, or claim that service-backed enforcement is active.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

use crate::{AppContext, Command, CommandErrorPayload, CommandResult, execute};

/// Version of the serialized service protocol.
pub const PROTOCOL_VERSION: u16 = 1;

/// Maximum JSON payload accepted in one frame, excluding the 4-byte header.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

const OVERSIZED_RESPONSE_CODE: &str = "response_too_large";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub version: u16,
    pub request_id: Uuid,
    pub command: Command,
}

impl RequestEnvelope {
    pub fn new(request_id: Uuid, command: Command) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            command,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub version: u16,
    pub request_id: Uuid,
    pub result: Result<CommandResult, CommandErrorPayload>,
}

impl ResponseEnvelope {
    pub fn new(request_id: Uuid, result: Result<CommandResult, CommandErrorPayload>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            request_id,
            result,
        }
    }
}

/// Read one big-endian length-prefixed JSON frame. Oversized or empty frames
/// are rejected before allocating their payload buffer.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(invalid_data(format!("invalid frame length: {length}")));
    }

    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(invalid_data)
}

/// Write one big-endian length-prefixed JSON frame, enforcing the same cap as
/// [`read_frame`].
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let payload = encode_payload(message).map_err(payload_error_to_io)?;
    write_encoded_frame(writer, &payload)
}

pub fn read_request<R: Read>(reader: &mut R) -> io::Result<RequestEnvelope> {
    let request: RequestEnvelope = read_frame(reader)?;
    validate_version(request.version)?;
    Ok(request)
}

pub fn write_request<W: Write>(writer: &mut W, request: &RequestEnvelope) -> io::Result<()> {
    validate_version(request.version)?;
    write_frame(writer, request)
}

pub fn read_response<R: Read>(reader: &mut R) -> io::Result<ResponseEnvelope> {
    let response: ResponseEnvelope = read_frame(reader)?;
    validate_version(response.version)?;
    Ok(response)
}

pub fn write_response<W: Write>(writer: &mut W, response: &ResponseEnvelope) -> io::Result<()> {
    validate_version(response.version)?;
    let payload = match encode_payload(response) {
        Ok(payload) => payload,
        Err(PayloadError::TooLarge) => {
            let fallback = ResponseEnvelope::new(
                response.request_id,
                Err(CommandErrorPayload {
                    code: OVERSIZED_RESPONSE_CODE.into(),
                    message: "Response exceeds the 1 MiB service protocol limit; narrow the query or split the operation.".into(),
                }),
            );
            encode_payload(&fallback).map_err(payload_error_to_io)?
        }
        Err(error) => return Err(payload_error_to_io(error)),
    };
    write_encoded_frame(writer, &payload)
}

/// Asynchronous counterpart to [`read_frame`], used by overlapped Windows
/// named-pipe transports so a slow or disconnected peer does not block a Tokio
/// worker thread.
pub async fn read_frame_async<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> io::Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header).await?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(invalid_data(format!("invalid frame length: {length}")));
    }

    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    serde_json::from_slice(&payload).map_err(invalid_data)
}

/// Asynchronous counterpart to [`write_frame`].
pub async fn write_frame_async<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    message: &T,
) -> io::Result<()> {
    let payload = encode_payload(message).map_err(payload_error_to_io)?;
    write_encoded_frame_async(writer, &payload).await
}

pub async fn read_request_async<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<RequestEnvelope> {
    let request: RequestEnvelope = read_frame_async(reader).await?;
    validate_version(request.version)?;
    Ok(request)
}

pub async fn write_request_async<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request: &RequestEnvelope,
) -> io::Result<()> {
    validate_version(request.version)?;
    write_frame_async(writer, request).await
}

pub async fn read_response_async<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> io::Result<ResponseEnvelope> {
    let response: ResponseEnvelope = read_frame_async(reader).await?;
    validate_version(response.version)?;
    Ok(response)
}

pub async fn write_response_async<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &ResponseEnvelope,
) -> io::Result<()> {
    validate_version(response.version)?;
    let payload = match encode_payload(response) {
        Ok(payload) => payload,
        Err(PayloadError::TooLarge) => {
            let fallback = ResponseEnvelope::new(
                response.request_id,
                Err(CommandErrorPayload {
                    code: OVERSIZED_RESPONSE_CODE.into(),
                    message: "Response exceeds the 1 MiB service protocol limit; narrow the query or split the operation.".into(),
                }),
            );
            encode_payload(&fallback).map_err(payload_error_to_io)?
        }
        Err(error) => return Err(payload_error_to_io(error)),
    };
    write_encoded_frame_async(writer, &payload).await
}

/// Execute an already decoded and authorized request through the shared command
/// layer. Only explicitly reviewed service commands are accepted. The pipe
/// adapter must authenticate its peer before calling this.
pub fn dispatch_service_request(
    context: &AppContext,
    request: RequestEnvelope,
) -> ResponseEnvelope {
    let result = if request.version != PROTOCOL_VERSION {
        Err(CommandErrorPayload {
            code: "unsupported_protocol_version".into(),
            message: format!("unsupported protocol version: {}", request.version),
        })
    } else if !service_command_allowed(&request.command) {
        Err(CommandErrorPayload {
            code: "command_not_supported_by_service".into(),
            message: "This command is only available to the desktop UI.".into(),
        })
    } else {
        execute(context, request.command).map_err(CommandErrorPayload::from)
    };
    ResponseEnvelope::new(request.request_id, result)
}

/// A closed allowlist: a newly added command is rejected until someone reviews
/// whether it is appropriate to expose through the privileged service.
fn service_command_allowed(command: &Command) -> bool {
    matches!(
        command,
        Command::ListBlockLists
            | Command::CreateBlockList { .. }
            | Command::UpdateBlockList { .. }
            | Command::DeleteBlockList { .. }
            | Command::ToggleBlockList { .. }
            | Command::AddWebsiteRule { .. }
            | Command::RemoveWebsiteRule { .. }
            | Command::BulkImportWebsites { .. }
            | Command::ClearAllWebsites
            | Command::AddAppRule { .. }
            | Command::RemoveAppRule { .. }
            | Command::ClearAllApps
            | Command::AddException { .. }
            | Command::RemoveException { .. }
            | Command::UpdateSchedule { .. }
            | Command::GetStats { .. }
            | Command::GetBlockedEvents { .. }
            | Command::ClearStatistics
            | Command::GetStatsRetention
            | Command::SetStatsRetention { .. }
            | Command::EnableProtection { .. }
            | Command::ConfigureScheduledProtection { .. }
            | Command::RelockScheduledProtection { .. }
            | Command::GetScheduledProtectionStatus
            | Command::ConfigureSharedAllowance { .. }
            | Command::GetSharedAllowanceStatus
            | Command::GetProtectionStatus
            | Command::RequestUnlockChallenge { .. }
            | Command::UnlockProtection { .. }
            | Command::GetSetting { .. }
            | Command::SetSetting { .. }
            | Command::ResetSettings
            | Command::GetBlockingHealth
            | Command::ApplyBlocks
            | Command::RemoveBlocks
            | Command::PomodoroStatus
            | Command::PomodoroPresets
            | Command::PomodoroStart { .. }
            | Command::PomodoroPause
            | Command::PomodoroResume
            | Command::PomodoroSkip
            | Command::PomodoroStop
            | Command::PomodoroDrainEvents
            | Command::PomodoroHistory { .. }
            | Command::AllowanceList
            | Command::AllowanceCreate { .. }
            | Command::AllowanceUpdate { .. }
            | Command::AllowanceDelete { .. }
            | Command::AllowanceResetToday { .. }
            | Command::AllowanceDrainNotifications
            | Command::AllowanceHistory { .. }
            | Command::ExportConfiguration
            | Command::ImportConfiguration { .. }
            | Command::DeleteAllData
            | Command::CheckDomain { .. }
            | Command::GetBrowserStatus
            | Command::AppVersion
    )
}

fn validate_version(version: u16) -> io::Result<()> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "unsupported protocol version: {version}"
        )))
    }
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

enum PayloadError {
    TooLarge,
    Serialization(serde_json::Error),
}

struct CappedPayloadWriter {
    bytes: Vec<u8>,
    exceeded_limit: bool,
}

impl Write for CappedPayloadWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded_limit = true;
            return Err(invalid_data("frame exceeds the 1 MiB payload limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_payload<T: Serialize>(message: &T) -> Result<Vec<u8>, PayloadError> {
    let mut writer = CappedPayloadWriter {
        bytes: Vec::new(),
        exceeded_limit: false,
    };
    if let Err(error) = serde_json::to_writer(&mut writer, message) {
        return if writer.exceeded_limit {
            Err(PayloadError::TooLarge)
        } else {
            Err(PayloadError::Serialization(error))
        };
    }
    if writer.bytes.is_empty() {
        return Err(PayloadError::Serialization(serde_json::Error::io(
            io::Error::new(io::ErrorKind::InvalidData, "empty JSON payload"),
        )));
    }
    Ok(writer.bytes)
}

fn payload_error_to_io(error: PayloadError) -> io::Error {
    match error {
        PayloadError::TooLarge => invalid_data("frame exceeds the 1 MiB payload limit"),
        PayloadError::Serialization(error) => invalid_data(error),
    }
}

fn write_encoded_frame<W: Write>(writer: &mut W, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| invalid_data("frame length cannot be represented"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(payload)
}

async fn write_encoded_frame_async<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| invalid_data("frame length cannot be represented"))?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(payload).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use focuser_core::{BlockEngine, Database};

    fn context() -> AppContext {
        AppContext::new_headless(BlockEngine::new(Database::open_in_memory().unwrap()).unwrap())
    }

    #[test]
    fn request_round_trips_with_version_and_request_id() {
        let request = RequestEnvelope::new(Uuid::new_v4(), Command::ListBlockLists);
        let mut bytes = Vec::new();
        write_request(&mut bytes, &request).unwrap();

        let decoded = read_request(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.version, PROTOCOL_VERSION);
        assert_eq!(decoded.request_id, request.request_id);
        assert!(matches!(decoded.command, Command::ListBlockLists));
    }

    #[tokio::test]
    async fn async_request_and_response_round_trip() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let request = RequestEnvelope::new(Uuid::new_v4(), Command::AppVersion);
        let expected_id = request.request_id;

        let client_task = async move {
            write_request_async(&mut client, &request).await.unwrap();
            let response = read_response_async(&mut client).await.unwrap();
            assert_eq!(response.request_id, expected_id);
            assert!(response.result.is_ok());
        };
        let server_task = async move {
            let request = read_request_async(&mut server).await.unwrap();
            assert!(matches!(request.command, Command::AppVersion));
            write_response_async(
                &mut server,
                &ResponseEnvelope::new(request.request_id, Ok(CommandResult::Unit)),
            )
            .await
            .unwrap();
        };
        tokio::join!(client_task, server_task);
    }

    #[tokio::test]
    async fn async_reader_rejects_oversized_frame_before_reading_payload() {
        let (mut writer, mut reader) = tokio::io::duplex(8);
        writer
            .write_all(&((MAX_FRAME_BYTES as u32 + 1).to_be_bytes()))
            .await
            .unwrap();

        let error = read_request_async(&mut reader).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn response_round_trips_typed_result() {
        let response = ResponseEnvelope::new(Uuid::new_v4(), Ok(CommandResult::Unit));
        let mut bytes = Vec::new();
        write_response(&mut bytes, &response).unwrap();

        let decoded = read_response(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request_id, response.request_id);
        assert!(matches!(decoded.result, Ok(CommandResult::Unit)));
    }

    #[test]
    fn dispatch_runs_only_versioned_commands_and_preserves_request_ids() {
        let context = context();
        let request_id = Uuid::new_v4();
        let response = dispatch_service_request(
            &context,
            RequestEnvelope::new(
                request_id,
                Command::CreateBlockList {
                    name: "Study".into(),
                },
            ),
        );
        assert_eq!(response.request_id, request_id);
        assert!(matches!(response.result, Ok(CommandResult::BlockList(_))));

        let request_id = Uuid::new_v4();
        let response = dispatch_service_request(
            &context,
            RequestEnvelope {
                version: PROTOCOL_VERSION + 1,
                request_id,
                command: Command::CreateBlockList {
                    name: "Must not be created".into(),
                },
            },
        );
        assert_eq!(response.request_id, request_id);
        assert!(matches!(
            response.result,
            Err(CommandErrorPayload { code, .. }) if code == "unsupported_protocol_version"
        ));
        assert_eq!(
            context
                .engine
                .lock()
                .unwrap()
                .db()
                .list_block_lists()
                .unwrap()
                .len(),
            1,
            "an unsupported version must not execute its command"
        );
    }

    #[test]
    fn dispatch_returns_stable_command_error_codes() {
        let context = context();
        let request_id = Uuid::new_v4();
        let response = dispatch_service_request(
            &context,
            RequestEnvelope::new(request_id, Command::DeleteBlockList { id: Uuid::new_v4() }),
        );
        assert_eq!(response.request_id, request_id);
        assert!(matches!(
            response.result,
            Err(CommandErrorPayload { code, .. }) if code == "block_list_not_found"
        ));
    }

    #[test]
    fn service_dispatch_rejects_ui_only_icon_loading() {
        let response = dispatch_service_request(
            &context(),
            RequestEnvelope::new(
                Uuid::new_v4(),
                Command::GetAppIcons {
                    targets: vec![r"C:\private\program.exe".into()],
                },
            ),
        );
        assert!(matches!(
            response.result,
            Err(CommandErrorPayload { code, .. }) if code == "command_not_supported_by_service"
        ));
    }

    #[test]
    fn unsupported_protocol_versions_are_rejected_on_read_and_write() {
        let mut bytes = Vec::new();
        write_frame(
            &mut bytes,
            &RequestEnvelope {
                version: PROTOCOL_VERSION + 1,
                request_id: Uuid::new_v4(),
                command: Command::ListBlockLists,
            },
        )
        .unwrap();
        assert_eq!(
            read_request(&mut bytes.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let request = RequestEnvelope {
            version: PROTOCOL_VERSION + 1,
            request_id: Uuid::new_v4(),
            command: Command::ListBlockLists,
        };
        assert_eq!(
            write_request(&mut Vec::new(), &request).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn oversized_frame_is_rejected_before_payload_read() {
        let header = ((MAX_FRAME_BYTES as u32) + 1).to_be_bytes();
        let mut bytes = header.as_slice();
        assert_eq!(
            read_frame::<_, RequestEnvelope>(&mut bytes)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn writes_are_capped_before_a_frame_header_is_sent() {
        let exactly_at_limit = "x".repeat(MAX_FRAME_BYTES - 2);
        let mut exact = Vec::new();
        write_frame(&mut exact, &exactly_at_limit).unwrap();
        assert_eq!(
            u32::from_be_bytes(exact[..4].try_into().unwrap()) as usize,
            MAX_FRAME_BYTES
        );
        assert_eq!(exact.len(), MAX_FRAME_BYTES + 4);

        let over_limit = "x".repeat(MAX_FRAME_BYTES - 1);
        let mut rejected = Vec::new();
        assert_eq!(
            write_frame(&mut rejected, &over_limit).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(rejected.is_empty(), "partial frames must not be emitted");
    }

    #[test]
    fn oversized_responses_return_a_small_correlated_error_frame() {
        let request_id = Uuid::new_v4();
        let response = ResponseEnvelope::new(
            request_id,
            Ok(CommandResult::Text("x".repeat(MAX_FRAME_BYTES))),
        );
        let mut bytes = Vec::new();
        write_response(&mut bytes, &response).unwrap();

        let decoded = read_response(&mut bytes.as_slice()).unwrap();
        assert_eq!(decoded.request_id, request_id);
        assert!(matches!(
            decoded.result,
            Err(CommandErrorPayload { code, .. }) if code == OVERSIZED_RESPONSE_CODE
        ));
    }

    #[test]
    fn truncated_headers_and_bodies_fail_with_unexpected_eof() {
        let mut short_header = [0, 0].as_slice();
        assert_eq!(
            read_frame::<_, RequestEnvelope>(&mut short_header)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut short_body = [0, 0, 0, 2, b'{'].as_slice();
        assert_eq!(
            read_frame::<_, RequestEnvelope>(&mut short_body)
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn empty_and_malformed_frames_are_rejected() {
        let mut empty = [0, 0, 0, 0].as_slice();
        assert_eq!(
            read_frame::<_, RequestEnvelope>(&mut empty)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let mut malformed = [0, 0, 0, 1, b'{'].as_slice();
        assert_eq!(
            read_frame::<_, RequestEnvelope>(&mut malformed)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
}
