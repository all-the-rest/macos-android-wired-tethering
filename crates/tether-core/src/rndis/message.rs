//! RNDIS control message codec.
//!
//! All multi-byte fields are little-endian.  Offset fields in control messages
//! and in `REMOTE_NDIS_PACKET_MSG` are relative to the start of the offset
//! field itself, i.e. **message start + 8** ([MS-RNDIS] 2.2.1, 2.2.2, 2.2.5,
//! 2.2.14).  `REMOTE_NDIS_INDICATE_STATUS_MSG` is the documented exception:
//! its status buffer offset is relative to the start of the message.
//!
//! Parsers never index past the supplied slice; every count and offset is
//! checked for overflow before use.

use crate::error::{Result, TetherError};

pub const MSG_PACKET: u32 = 0x0000_0001;
pub const MSG_INITIALIZE: u32 = 0x0000_0002;
pub const MSG_HALT: u32 = 0x0000_0003;
pub const MSG_QUERY: u32 = 0x0000_0004;
pub const MSG_SET: u32 = 0x0000_0005;
pub const MSG_RESET: u32 = 0x0000_0006;
pub const MSG_INDICATE_STATUS: u32 = 0x0000_0007;
pub const MSG_KEEPALIVE: u32 = 0x0000_0008;

pub const MSG_INITIALIZE_CMPLT: u32 = 0x8000_0002;
pub const MSG_QUERY_CMPLT: u32 = 0x8000_0004;
pub const MSG_SET_CMPLT: u32 = 0x8000_0005;
pub const MSG_RESET_CMPLT: u32 = 0x8000_0006;
pub const MSG_KEEPALIVE_CMPLT: u32 = 0x8000_0008;

pub const HEADER_LEN: usize = 12;
pub const PACKET_HEADER_LEN: usize = 44;
pub const PACKET_INLINE_DATA_OFFSET: u32 = 36; // 44 - 8, see module docs
pub const QUERY_INLINE_DATA_OFFSET: u32 = 20; // 28 - 8
pub const QUERY_COMPLETE_INLINE_DATA_OFFSET: u32 = 16; // 24 - 8

pub const STATUS_SUCCESS: u32 = 0x0000_0000;
pub const STATUS_PENDING: u32 = 0x0000_0103;
pub const STATUS_MEDIA_CONNECT: u32 = 0x4001_000b;
pub const STATUS_MEDIA_DISCONNECT: u32 = 0x4001_000c;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u32,
    pub length: usize,
    pub request_id: u32,
}

pub fn parse_header(buf: &[u8]) -> Result<Header> {
    if buf.len() < HEADER_LEN {
        return Err(TetherError::protocol("RNDIS message shorter than 12 bytes"));
    }
    let length = le_u32(&buf[4..8]) as usize;
    if length < HEADER_LEN {
        return Err(TetherError::protocol(
            "RNDIS MessageLength is smaller than the header",
        ));
    }
    if length > buf.len() {
        return Err(TetherError::protocol(
            "RNDIS MessageLength exceeds the received buffer",
        ));
    }
    Ok(Header {
        kind: le_u32(&buf[0..4]),
        length,
        request_id: le_u32(&buf[8..12]),
    })
}

pub fn encode_header(kind: u32, length: usize, request_id: u32) -> Result<[u8; HEADER_LEN]> {
    let length_u32 = u32::try_from(length)
        .map_err(|_| TetherError::protocol("RNDIS message length does not fit in 32 bits"))?;
    let mut out = [0u8; HEADER_LEN];
    out[0..4].copy_from_slice(&kind.to_le_bytes());
    out[4..8].copy_from_slice(&length_u32.to_le_bytes());
    out[8..12].copy_from_slice(&request_id.to_le_bytes());
    Ok(out)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitializeComplete {
    pub request_id: u32,
    pub status: u32,
    pub major_version: u32,
    pub minor_version: u32,
    pub device_flags: u32,
    pub medium: u32,
    pub max_packets_per_transfer: u32,
    pub max_transfer_size: u32,
    pub packet_alignment_exponent: u32,
    pub af_list_offset: u32,
    pub af_list_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryComplete<'a> {
    pub request_id: u32,
    pub status: u32,
    pub information: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetComplete {
    pub request_id: u32,
    pub status: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetComplete {
    pub status: u32,
    pub addressing_reset: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeepAliveComplete {
    pub request_id: u32,
    pub status: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicateStatus<'a> {
    pub status: u32,
    pub buffer: Option<&'a [u8]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet<'a> {
    pub message_length: usize,
    /// Absolute offset of the packet data from the start of the message.
    pub data_offset: usize,
    pub data_length: usize,
    /// Packet data.  The slice is exactly `data_length` bytes.
    pub data: &'a [u8],
    pub oob_data_offset: u32,
    pub oob_data_length: u32,
    pub per_packet_info_offset: u32,
    pub per_packet_info_length: u32,
}

pub fn parse_packet(buf: &[u8]) -> Result<Packet<'_>> {
    if buf.len() < PACKET_HEADER_LEN {
        return Err(TetherError::protocol(
            "RNDIS packet message shorter than 44 bytes",
        ));
    }
    let kind = le_u32(&buf[0..4]);
    if kind != MSG_PACKET {
        return Err(TetherError::protocol("not an RNDIS packet message"));
    }
    let message_length = le_u32(&buf[4..8]) as usize;
    if message_length < PACKET_HEADER_LEN || message_length > buf.len() {
        return Err(TetherError::protocol("invalid RNDIS packet message length"));
    }
    let data_offset_field = le_u32(&buf[8..12]);
    if data_offset_field % 4 != 0 {
        return Err(TetherError::protocol(
            "RNDIS packet DataOffset is not 32-bit aligned",
        ));
    }
    let data_length = le_u32(&buf[12..16]) as usize;
    let data_offset = 8usize
        .checked_add(data_offset_field as usize)
        .ok_or_else(|| TetherError::protocol("RNDIS packet DataOffset overflow"))?;
    if data_offset < PACKET_HEADER_LEN {
        return Err(TetherError::protocol(
            "RNDIS packet DataOffset points into the header",
        ));
    }
    let data_end = data_offset
        .checked_add(data_length)
        .ok_or_else(|| TetherError::protocol("RNDIS packet data length overflow"))?;
    if data_end > message_length {
        return Err(TetherError::protocol(
            "RNDIS packet data exceeds MessageLength",
        ));
    }
    let oob_data_offset = le_u32(&buf[16..20]);
    let oob_data_length = le_u32(&buf[20..24]);
    let per_packet_info_offset = le_u32(&buf[28..32]);
    let per_packet_info_length = le_u32(&buf[32..36]);
    if oob_data_length != 0 {
        let start = 8usize
            .checked_add(oob_data_offset as usize)
            .ok_or_else(|| TetherError::protocol("RNDIS OOB offset overflow"))?;
        let end = start
            .checked_add(oob_data_length as usize)
            .ok_or_else(|| TetherError::protocol("RNDIS OOB length overflow"))?;
        if start < PACKET_HEADER_LEN || end > message_length {
            return Err(TetherError::protocol(
                "RNDIS out-of-band data is outside the message",
            ));
        }
    }
    if per_packet_info_length != 0 {
        let start = 8usize
            .checked_add(per_packet_info_offset as usize)
            .ok_or_else(|| TetherError::protocol("RNDIS per-packet offset overflow"))?;
        let end = start
            .checked_add(per_packet_info_length as usize)
            .ok_or_else(|| TetherError::protocol("RNDIS per-packet length overflow"))?;
        if start < PACKET_HEADER_LEN || end > message_length {
            return Err(TetherError::protocol(
                "RNDIS per-packet info is outside the message",
            ));
        }
    }
    Ok(Packet {
        message_length,
        data_offset,
        data_length,
        data: &buf[data_offset..data_end],
        oob_data_offset,
        oob_data_length,
        per_packet_info_offset,
        per_packet_info_length,
    })
}

/// Split one USB bulk transfer into all packet messages it contains.
///
/// A transfer may end with up to three zero padding bytes or a one-byte USB
/// short-packet pad; these are tolerated only when the remainder is all zero.
pub fn split_packet_transfer(buf: &[u8]) -> Result<Vec<Packet<'_>>> {
    let mut packets = Vec::new();
    let mut offset = 0usize;
    while offset < buf.len() {
        let remaining = &buf[offset..];
        if remaining.len() < PACKET_HEADER_LEN {
            if remaining.iter().all(|&b| b == 0) {
                break;
            }
            return Err(TetherError::protocol(
                "trailing bytes are too short for an RNDIS packet header",
            ));
        }
        let packet = parse_packet(remaining)?;
        if packet.message_length == 0 {
            return Err(TetherError::protocol("zero-length RNDIS packet message"));
        }
        let length = packet.message_length;
        packets.push(packet);
        offset += length;
        if packets.len() > 256 {
            return Err(TetherError::protocol(
                "too many RNDIS packets in one transfer",
            ));
        }
    }
    Ok(packets)
}

pub fn encode_initialize_request(request_id: u32, max_transfer_size: u32) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(24);
    out.extend_from_slice(&encode_header(MSG_INITIALIZE, 24, request_id)?);
    out.extend_from_slice(&1u32.to_le_bytes()); // major version 1
    out.extend_from_slice(&0u32.to_le_bytes()); // minor version 0
    out.extend_from_slice(&max_transfer_size.to_le_bytes());
    debug_assert_eq!(out.len(), 24);
    Ok(out)
}

pub fn encode_query_request(request_id: u32, oid: u32, information: &[u8]) -> Result<Vec<u8>> {
    encode_oid_request(MSG_QUERY, request_id, oid, information)
}

pub fn encode_set_request(request_id: u32, oid: u32, information: &[u8]) -> Result<Vec<u8>> {
    encode_oid_request(MSG_SET, request_id, oid, information)
}

fn encode_oid_request(kind: u32, request_id: u32, oid: u32, information: &[u8]) -> Result<Vec<u8>> {
    let length = 28usize
        .checked_add(information.len())
        .ok_or_else(|| TetherError::protocol("RNDIS OID request length overflow"))?;
    let mut out = Vec::with_capacity(length);
    out.extend_from_slice(&encode_header(kind, length, request_id)?);
    out.extend_from_slice(&oid.to_le_bytes());
    out.extend_from_slice(
        &u32::try_from(information.len())
            .map_err(|_| TetherError::protocol("OID information buffer too large"))?
            .to_le_bytes(),
    );
    out.extend_from_slice(&QUERY_INLINE_DATA_OFFSET.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // DeviceVcHandle
    out.extend_from_slice(information);
    debug_assert_eq!(out.len(), length);
    Ok(out)
}

pub fn encode_reset_request() -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&encode_header(MSG_RESET, 12, 0)?);
    Ok(out)
}

pub fn encode_halt_request(request_id: u32) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&encode_header(MSG_HALT, 12, request_id)?);
    Ok(out)
}

pub fn encode_keepalive_request(request_id: u32) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&encode_header(MSG_KEEPALIVE, 12, request_id)?);
    Ok(out)
}

pub fn parse_initialize_complete(buf: &[u8]) -> Result<InitializeComplete> {
    let header = parse_header(buf)?;
    if header.kind != MSG_INITIALIZE_CMPLT {
        return Err(TetherError::protocol("not an RNDIS initialize completion"));
    }
    if header.length < 52 || buf.len() < 52 {
        return Err(TetherError::protocol(
            "truncated RNDIS initialize completion",
        ));
    }
    let packet_alignment_exponent = le_u32(&buf[40..44]);
    if packet_alignment_exponent > 7 {
        return Err(TetherError::protocol(
            "RNDIS PacketAlignmentFactor exponent exceeds 7",
        ));
    }
    Ok(InitializeComplete {
        request_id: header.request_id,
        status: le_u32(&buf[12..16]),
        major_version: le_u32(&buf[16..20]),
        minor_version: le_u32(&buf[20..24]),
        device_flags: le_u32(&buf[24..28]),
        medium: le_u32(&buf[28..32]),
        max_packets_per_transfer: le_u32(&buf[32..36]),
        max_transfer_size: le_u32(&buf[36..40]),
        packet_alignment_exponent,
        af_list_offset: le_u32(&buf[44..48]),
        af_list_size: le_u32(&buf[48..52]),
    })
}

pub fn parse_query_complete(buf: &[u8]) -> Result<QueryComplete<'_>> {
    let header = parse_header(buf)?;
    if header.kind != MSG_QUERY_CMPLT {
        return Err(TetherError::protocol("not an RNDIS query completion"));
    }
    if header.length < 24 || buf.len() < 24 {
        return Err(TetherError::protocol("truncated RNDIS query completion"));
    }
    let status = le_u32(&buf[12..16]);
    let length = le_u32(&buf[16..20]) as usize;
    let offset_field = le_u32(&buf[20..24]) as usize;
    let data_offset = 8usize
        .checked_add(offset_field)
        .ok_or_else(|| TetherError::protocol("RNDIS query offset overflow"))?;
    let data_end = data_offset
        .checked_add(length)
        .ok_or_else(|| TetherError::protocol("RNDIS query length overflow"))?;
    if length != 0 && (data_offset < 24 || data_end > header.length) {
        return Err(TetherError::protocol(
            "RNDIS query information buffer is outside the message",
        ));
    }
    let information = if length == 0 {
        &[][..]
    } else {
        &buf[data_offset..data_end]
    };
    Ok(QueryComplete {
        request_id: header.request_id,
        status,
        information,
    })
}

pub fn parse_set_complete(buf: &[u8]) -> Result<SetComplete> {
    let header = parse_header(buf)?;
    if header.kind != MSG_SET_CMPLT {
        return Err(TetherError::protocol("not an RNDIS set completion"));
    }
    if header.length < 16 || buf.len() < 16 {
        return Err(TetherError::protocol("truncated RNDIS set completion"));
    }
    Ok(SetComplete {
        request_id: header.request_id,
        status: le_u32(&buf[12..16]),
    })
}

pub fn parse_reset_complete(buf: &[u8]) -> Result<ResetComplete> {
    let header = parse_header(buf)?;
    if header.kind != MSG_RESET_CMPLT {
        return Err(TetherError::protocol("not an RNDIS reset completion"));
    }
    if header.length < 16 || buf.len() < 16 {
        return Err(TetherError::protocol("truncated RNDIS reset completion"));
    }
    Ok(ResetComplete {
        status: le_u32(&buf[8..12]),
        addressing_reset: le_u32(&buf[12..16]),
    })
}

pub fn parse_keepalive_complete(buf: &[u8]) -> Result<KeepAliveComplete> {
    let header = parse_header(buf)?;
    if header.kind != MSG_KEEPALIVE_CMPLT {
        return Err(TetherError::protocol("not an RNDIS keepalive completion"));
    }
    if header.length < 16 || buf.len() < 16 {
        return Err(TetherError::protocol(
            "truncated RNDIS keepalive completion",
        ));
    }
    Ok(KeepAliveComplete {
        request_id: header.request_id,
        status: le_u32(&buf[12..16]),
    })
}

pub fn parse_indicate_status(buf: &[u8]) -> Result<IndicateStatus<'_>> {
    let header = parse_header(buf)?;
    if header.kind != MSG_INDICATE_STATUS {
        return Err(TetherError::protocol("not an RNDIS status indication"));
    }
    if header.length < 20 || buf.len() < 20 {
        return Err(TetherError::protocol("truncated RNDIS status indication"));
    }
    let length = le_u32(&buf[12..16]) as usize;
    let offset = le_u32(&buf[16..20]) as usize;
    let buffer = if length == 0 {
        Some(&[][..])
    } else {
        // INDICATE_STATUS is the documented exception: offset is relative to
        // the start of the message, not message start + 8.
        offset
            .checked_add(length)
            .filter(|&end| offset >= 20 && end <= header.length)
            .map(|end| &buf[offset..end])
    };
    Ok(IndicateStatus {
        status: le_u32(&buf[8..12]),
        buffer,
    })
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Independently written from the field offsets in [MS-RNDIS] 2.2.2.
    fn initialize_complete_fixture() -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(&MSG_INITIALIZE_CMPLT.to_le_bytes());
        data.extend_from_slice(&52u32.to_le_bytes());
        data.extend_from_slice(&7u32.to_le_bytes()); // request id
        data.extend_from_slice(&0u32.to_le_bytes()); // success
        data.extend_from_slice(&1u32.to_le_bytes()); // major
        data.extend_from_slice(&0u32.to_le_bytes()); // minor
        data.extend_from_slice(&1u32.to_le_bytes()); // connectionless
        data.extend_from_slice(&0u32.to_le_bytes()); // 802.3
        data.extend_from_slice(&4u32.to_le_bytes()); // max packets
        data.extend_from_slice(&0x4000u32.to_le_bytes()); // max transfer
        data.extend_from_slice(&2u32.to_le_bytes()); // 4-byte alignment
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data
    }

    #[test]
    fn initialize_request_layout() {
        let request = encode_initialize_request(0x1234_5678, 0x4000).unwrap();
        assert_eq!(request.len(), 24);
        assert_eq!(&request[0..4], &MSG_INITIALIZE.to_le_bytes());
        assert_eq!(&request[4..8], &24u32.to_le_bytes());
        assert_eq!(&request[8..12], &0x1234_5678u32.to_le_bytes());
        assert_eq!(&request[12..16], &1u32.to_le_bytes());
        assert_eq!(&request[16..20], &0u32.to_le_bytes());
        assert_eq!(&request[20..24], &0x4000u32.to_le_bytes());
    }

    #[test]
    fn initialize_complete_layout() {
        let parsed = parse_initialize_complete(&initialize_complete_fixture()).unwrap();
        assert_eq!(parsed.request_id, 7);
        assert_eq!(parsed.status, 0);
        assert_eq!(parsed.medium, 0);
        assert_eq!(parsed.max_packets_per_transfer, 4);
        assert_eq!(parsed.max_transfer_size, 0x4000);
        assert_eq!(parsed.packet_alignment_exponent, 2);
    }

    #[test]
    fn query_request_uses_offset_20() {
        let request = encode_query_request(9, 0x0001_0101, &[]).unwrap();
        assert_eq!(request.len(), 28);
        assert_eq!(&request[12..16], &0x0001_0101u32.to_le_bytes());
        assert_eq!(&request[16..20], &0u32.to_le_bytes());
        assert_eq!(&request[20..24], &20u32.to_le_bytes());
        assert_eq!(&request[24..28], &0u32.to_le_bytes());
    }

    #[test]
    fn query_complete_offset_is_relative_to_field_base() {
        let mut response = Vec::new();
        response.extend_from_slice(&MSG_QUERY_CMPLT.to_le_bytes());
        response.extend_from_slice(&28u32.to_le_bytes());
        response.extend_from_slice(&11u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&4u32.to_le_bytes());
        response.extend_from_slice(&16u32.to_le_bytes()); // absolute 24
        response.extend_from_slice(&1500u32.to_le_bytes());
        let parsed = parse_query_complete(&response).unwrap();
        assert_eq!(parsed.information, &1500u32.to_le_bytes());
        assert_eq!(parsed.request_id, 11);
    }

    #[test]
    fn reset_completion_status_is_at_offset_8() {
        let mut response = Vec::new();
        response.extend_from_slice(&MSG_RESET_CMPLT.to_le_bytes());
        response.extend_from_slice(&16u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        response.extend_from_slice(&1u32.to_le_bytes());
        let parsed = parse_reset_complete(&response).unwrap();
        assert_eq!(parsed.status, 0);
        assert_eq!(parsed.addressing_reset, 1);
    }

    #[test]
    fn indicate_status_buffer_offset_is_from_message_start() {
        let mut response = Vec::new();
        response.extend_from_slice(&MSG_INDICATE_STATUS.to_le_bytes());
        response.extend_from_slice(&24u32.to_le_bytes());
        response.extend_from_slice(&STATUS_MEDIA_CONNECT.to_le_bytes());
        response.extend_from_slice(&4u32.to_le_bytes());
        response.extend_from_slice(&20u32.to_le_bytes());
        response.extend_from_slice(&0u32.to_le_bytes());
        let parsed = parse_indicate_status(&response).unwrap();
        assert_eq!(parsed.status, STATUS_MEDIA_CONNECT);
        assert_eq!(parsed.buffer, Some(&0u32.to_le_bytes()[..]));
    }

    #[test]
    fn rejects_truncated_messages() {
        assert!(parse_header(&[0u8; 11]).is_err());
        assert!(parse_initialize_complete(&initialize_complete_fixture()[..51]).is_err());
        assert!(parse_packet(&[0u8; 43]).is_err());
    }

    #[test]
    fn packet_roundtrip_and_alignment() {
        let frame = vec![0x11u8; 60];
        let mut encoded = Vec::new();
        encoded.extend_from_slice(&MSG_PACKET.to_le_bytes());
        encoded.extend_from_slice(&104u32.to_le_bytes()); // 44 + 60, already aligned
        encoded.extend_from_slice(&36u32.to_le_bytes());
        encoded.extend_from_slice(&60u32.to_le_bytes());
        encoded.extend_from_slice(&[0u8; 28]);
        encoded.extend_from_slice(&frame);
        let packets = split_packet_transfer(&encoded).unwrap();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].data, frame.as_slice());
        assert_eq!(packets[0].data_offset, 44);
        assert_eq!(packets[0].message_length, 104);
    }

    #[test]
    fn rejects_packet_data_offset_into_header() {
        let mut packet = Vec::new();
        packet.extend_from_slice(&MSG_PACKET.to_le_bytes());
        packet.extend_from_slice(&44u32.to_le_bytes());
        packet.extend_from_slice(&4u32.to_le_bytes()); // absolute 12
        packet.extend_from_slice(&1u32.to_le_bytes());
        packet.extend_from_slice(&[0u8; 32]);
        assert!(parse_packet(&packet).is_err());
    }

    #[test]
    fn rejects_packet_length_overflow() {
        let mut packet = vec![0u8; 44];
        packet[0..4].copy_from_slice(&MSG_PACKET.to_le_bytes());
        packet[4..8].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
        assert!(parse_packet(&packet).is_err());
    }
}
