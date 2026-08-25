use bytes::{Buf, BufMut, Bytes, BytesMut};
use thiserror::Error;

pub const TYPE_HELLO: u8 = 0x01;
pub const TYPE_META: u8 = 0x02;
pub const TYPE_SYMBOL: u8 = 0x03;
pub const TYPE_DONE: u8 = 0x04;
pub const TYPE_READY: u8 = 0x05;
pub const TYPE_DIRECT: u8 = 0x06;

pub const ROLE_SENDER: u8 = 0x01;
pub const ROLE_RECEIVER: u8 = 0x02;

pub const MAX_FRAME: u32 = 512 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("invalid message type: {0}")]
    InvalidType(u8),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(u32),
    #[error("truncated message")]
    Truncated,
    #[error("invalid utf-8 filename")]
    InvalidFilename,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrbitMessage {
    Hello { session_id: u64, role: u8 },
    Meta {
        session_id: u64,
        filename: String,
        size: u64,
        symbol_size: u32,
        k: u32,
        checksum: u64,
    },
    Symbol {
        session_id: u64,
        esi: u32,
        data: Bytes,
    },
    Done { session_id: u64 },
    Ready { session_id: u64 },
    Direct { session_id: u64, addr: String },
}

impl OrbitMessage {
    pub fn session_id(&self) -> u64 {
        match self {
            OrbitMessage::Hello { session_id, .. }
            | OrbitMessage::Meta { session_id, .. }
            | OrbitMessage::Symbol { session_id, .. }
            | OrbitMessage::Done { session_id }
            | OrbitMessage::Ready { session_id }
            | OrbitMessage::Direct { session_id, .. } => *session_id,
        }
    }

    pub fn encode(&self) -> BytesMut {
        let ty;
        let mut payload = BytesMut::new();
        match self {
            OrbitMessage::Hello { session_id, role } => {
                ty = TYPE_HELLO;
                payload.put_u64_le(*session_id);
                payload.put_u8(*role);
            }
            OrbitMessage::Meta {
                session_id,
                filename,
                size,
                symbol_size,
                k,
                checksum,
            } => {
                ty = TYPE_META;
                payload.put_u64_le(*session_id);
                payload.put_u16_le(filename.len() as u16);
                payload.put_slice(filename.as_bytes());
                payload.put_u64_le(*size);
                payload.put_u32_le(*symbol_size);
                payload.put_u32_le(*k);
                payload.put_u64_le(*checksum);
            }
            OrbitMessage::Symbol {
                session_id,
                esi,
                data,
            } => {
                ty = TYPE_SYMBOL;
                payload.put_u64_le(*session_id);
                payload.put_u32_le(*esi);
                payload.put_slice(data);
            }
            OrbitMessage::Done { session_id } => {
                ty = TYPE_DONE;
                payload.put_u64_le(*session_id);
            }
            OrbitMessage::Ready { session_id } => {
                ty = TYPE_READY;
                payload.put_u64_le(*session_id);
            }
            OrbitMessage::Direct { session_id, addr } => {
                ty = TYPE_DIRECT;
                payload.put_u64_le(*session_id);
                payload.put_u16_le(addr.len() as u16);
                payload.put_slice(addr.as_bytes());
            }
        }

        let mut frame = BytesMut::with_capacity(5 + payload.len());
        frame.put_u8(ty);
        frame.put_u32_le(payload.len() as u32);
        frame.put_slice(&payload);
        frame
    }

    pub fn decode(frame: &[u8]) -> Result<OrbitMessage, ProtocolError> {
        if frame.len() < 5 {
            return Err(ProtocolError::Truncated);
        }
        let ty = frame[0];
        let len = u32::from_le_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        if frame.len() != 5 + len {
            return Err(ProtocolError::Truncated);
        }
        let mut b = Bytes::copy_from_slice(&frame[5..]);

        match ty {
            TYPE_HELLO => {
                if b.len() < 9 {
                    return Err(ProtocolError::Truncated);
                }
                let session_id = b.get_u64_le();
                let role = b.get_u8();
                Ok(OrbitMessage::Hello { session_id, role })
            }
            TYPE_META => {
                if b.len() < 10 {
                    return Err(ProtocolError::Truncated);
                }
                let session_id = b.get_u64_le();
                let name_len = b.get_u16_le() as usize;
                if b.len() < name_len + 20 {
                    return Err(ProtocolError::Truncated);
                }
                let filename = String::from_utf8(b.split_to(name_len).to_vec())
                    .map_err(|_| ProtocolError::InvalidFilename)?;
                let size = b.get_u64_le();
                let symbol_size = b.get_u32_le();
                let k = b.get_u32_le();
                let checksum = b.get_u64_le();
                Ok(OrbitMessage::Meta {
                    session_id,
                    filename,
                    size,
                    symbol_size,
                    k,
                    checksum,
                })
            }
            TYPE_SYMBOL => {
                if b.len() < 12 {
                    return Err(ProtocolError::Truncated);
                }
                let session_id = b.get_u64_le();
                let esi = b.get_u32_le();
                let data = b;
                Ok(OrbitMessage::Symbol {
                    session_id,
                    esi,
                    data,
                })
            }
            TYPE_DONE => {
                if b.len() < 8 {
                    return Err(ProtocolError::Truncated);
                }
                Ok(OrbitMessage::Done {
                    session_id: b.get_u64_le(),
                })
            }
            TYPE_READY => {
                if b.len() < 8 {
                    return Err(ProtocolError::Truncated);
                }
                Ok(OrbitMessage::Ready {
                    session_id: b.get_u64_le(),
                })
            }
            TYPE_DIRECT => {
                if b.len() < 10 {
                    return Err(ProtocolError::Truncated);
                }
                let session_id = b.get_u64_le();
                let addr_len = b.get_u16_le() as usize;
                if b.len() < addr_len {
                    return Err(ProtocolError::Truncated);
                }
                let addr = String::from_utf8(b.split_to(addr_len).to_vec())
                    .map_err(|_| ProtocolError::InvalidFilename)?;
                Ok(OrbitMessage::Direct { session_id, addr })
            }
            other => Err(ProtocolError::InvalidType(other)),
        }
    }

    /// Attempts to parse a complete frame from `buf`, consuming it on success.
    pub fn parse_frame(buf: &mut BytesMut) -> Result<Option<OrbitMessage>, ProtocolError> {
        if buf.len() < 5 {
            return Ok(None);
        }
        let len = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if len > MAX_FRAME as usize {
            return Err(ProtocolError::FrameTooLarge(len as u32));
        }
        if buf.len() < 5 + len {
            return Ok(None);
        }
        let frame = buf.split_to(5 + len);
        OrbitMessage::decode(&frame).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_all_message_types() {
        let msgs = vec![
            OrbitMessage::Hello {
                session_id: 42,
                role: ROLE_SENDER,
            },
            OrbitMessage::Meta {
                session_id: 42,
                filename: "video-4k.mp4".to_string(),
                size: 1_000_000_000,
                symbol_size: 65536,
                k: 15259,
                checksum: 0xDEADBEEF,
            },
            OrbitMessage::Symbol {
                session_id: 42,
                esi: 1234,
                data: Bytes::from(vec![0xAB; 4096]),
            },
            OrbitMessage::Done { session_id: 42 },
            OrbitMessage::Ready { session_id: 42 },
            OrbitMessage::Direct {
                session_id: 42,
                addr: "127.0.0.1:44321".to_string(),
            },
        ];
        for msg in msgs {
            let encoded = msg.encode();
            let decoded = OrbitMessage::decode(&encoded).unwrap();
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn parse_frame_handles_partial_data() {
        let mut full = OrbitMessage::Symbol {
            session_id: 1,
            esi: 2,
            data: Bytes::from(vec![7u8; 100]),
        }
        .encode();

        let mut partial = full.split_to(10);
        assert!(matches!(
            OrbitMessage::parse_frame(&mut partial).unwrap(),
            None
        ));
        partial.extend_from_slice(&full);
        assert!(matches!(
            OrbitMessage::parse_frame(&mut partial).unwrap(),
            Some(OrbitMessage::Symbol { .. })
        ));
    }

    #[test]
    fn rejects_oversized_frame() {
        let mut buf = BytesMut::new();
        buf.put_u8(TYPE_SYMBOL);
        buf.put_u32_le(MAX_FRAME + 1);
        assert!(matches!(
            OrbitMessage::parse_frame(&mut buf),
            Err(ProtocolError::FrameTooLarge(_))
        ));
    }
}