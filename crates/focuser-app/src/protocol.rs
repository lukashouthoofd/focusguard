//! Bounded wire framing for the future Windows service boundary.
//!
//! This module only defines and validates messages. It does not create a pipe,
//! authenticate a peer, or claim that service-backed enforcement is active.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use uuid::Uuid;

use crate::{Command, CommandErrorPayload, CommandResult};

/// Version of the serialized service protocol.
pub const PROTOCOL_VERSION: u16 = 1;

/// Maximum JSON payload accepted in one frame, excluding the 4-byte header.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

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
    let payload = serde_json::to_vec(message).map_err(invalid_data)?;
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES {
        return Err(invalid_data(format!(
            "invalid frame length: {}",
            payload.len()
        )));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| invalid_data("frame length cannot be represented"))?;
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)
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
    write_frame(writer, response)
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

#[cfg(test)]
mod tests {
    use super::*;

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
