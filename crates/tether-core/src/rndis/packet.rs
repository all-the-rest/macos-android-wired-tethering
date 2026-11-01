//! RNDIS packet-message framing and aggregation.
//!
//! `REMOTE_NDIS_PACKET_MSG` has a 44-byte header.  `DataOffset` is relative to
//! the start of the `DataOffset` field (message start + 8), so inline data uses
//! the value 36.  `MessageLength` includes padding that may be appended to a
//! message; the final message of a USB transfer may be followed by one extra
//! zero byte (USB short-packet workaround).

use crate::error::{Result, TetherError};
use crate::rndis::message::{self, PACKET_HEADER_LEN, PACKET_INLINE_DATA_OFFSET};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedTransfer {
    pub bytes: Vec<u8>,
    pub frames: usize,
}

/// Encode one Ethernet frame as a single RNDIS packet message.
pub fn encode_packet_message(frame: &[u8], alignment: usize) -> Result<Vec<u8>> {
    if frame.is_empty() {
        return Err(TetherError::protocol(
            "refusing to send an empty Ethernet frame",
        ));
    }
    if frame.len() > 0xffff - PACKET_HEADER_LEN {
        return Err(TetherError::protocol(
            "Ethernet frame is too large for RNDIS",
        ));
    }
    let alignment = alignment.max(1);
    if !alignment.is_power_of_two() {
        return Err(TetherError::protocol(
            "RNDIS packet alignment is not a power of two",
        ));
    }
    let unpadded = PACKET_HEADER_LEN
        .checked_add(frame.len())
        .ok_or_else(|| TetherError::protocol("RNDIS packet length overflow"))?;
    let message_length = align_up(unpadded, alignment)?;
    let mut out = Vec::with_capacity(message_length);
    out.extend_from_slice(&message::MSG_PACKET.to_le_bytes());
    out.extend_from_slice(&(message_length as u32).to_le_bytes());
    out.extend_from_slice(&PACKET_INLINE_DATA_OFFSET.to_le_bytes());
    out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 28]); // OOB/per-packet/VcHandle/reserved
    out.extend_from_slice(frame);
    out.resize(message_length, 0);
    Ok(out)
}

/// Aggregate frames into one USB transfer.  Stops before a frame that would
/// exceed `max_transfer` or `max_packets`; returns the number of frames
/// consumed so the caller can send the rest in the next transfer.
pub fn encode_transfer(
    frames: &[&[u8]],
    max_packets: usize,
    max_transfer: usize,
    alignment: usize,
    bulk_max_packet: usize,
) -> Result<EncodedTransfer> {
    if frames.is_empty() {
        return Ok(EncodedTransfer {
            bytes: Vec::new(),
            frames: 0,
        });
    }
    let max_packets = max_packets.max(1);
    let mut bytes = Vec::new();
    let mut consumed = 0usize;
    for frame in frames {
        if consumed >= max_packets {
            break;
        }
        let message = encode_packet_message(frame, alignment)?;
        if bytes.len() + message.len() > max_transfer {
            if consumed == 0 {
                return Err(TetherError::new(
                    crate::error::ErrorCategory::ProtocolViolation,
                    format!(
                        "frame of {} bytes does not fit into MaxTransferSize {}",
                        frame.len(),
                        max_transfer
                    ),
                ));
            }
            break;
        }
        bytes.extend_from_slice(&message);
        consumed += 1;
    }
    // A transfer whose length is an exact multiple of the endpoint's
    // wMaxPacketSize would be interpreted as a ZLP candidate.  RNDIS devices
    // must tolerate one appended byte, and [MS-RNDIS] USB Short Packets says
    // the host appends a one-byte zero packet.  Do that instead of a real ZLP.
    if bulk_max_packet > 0 && !bytes.is_empty() && bytes.len() % bulk_max_packet == 0 {
        bytes.push(0);
    }
    Ok(EncodedTransfer {
        bytes,
        frames: consumed,
    })
}

fn align_up(value: usize, alignment: usize) -> Result<usize> {
    let mask = alignment - 1;
    value
        .checked_add(mask)
        .map(|v| v & !mask)
        .ok_or_else(|| TetherError::protocol("RNDIS alignment overflow"))
}

/// Reassembles packet messages that may be split across USB transfers and
/// tolerates trailing zero padding.  The buffer is bounded to avoid a
/// malicious device growing memory without limit.
#[derive(Debug, Default)]
pub struct PacketReassembler {
    buffer: Vec<u8>,
    max_buffer: usize,
}

impl PacketReassembler {
    pub fn new(max_buffer: usize) -> Self {
        Self {
            buffer: Vec::new(),
            max_buffer: max_buffer.max(PACKET_HEADER_LEN * 2),
        }
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        if self.buffer.len() + data.len() > self.max_buffer {
            self.buffer.clear();
            return Err(TetherError::protocol(
                "RNDIS receive reassembly buffer exceeded the safety limit",
            ));
        }
        self.buffer.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            if self.buffer.is_empty() {
                break;
            }
            if self.buffer.len() < PACKET_HEADER_LEN {
                // A trailing USB short-packet pad is a single zero byte.  Only
                // discard when everything left is zero.
                if self.buffer.iter().all(|&b| b == 0) {
                    self.buffer.clear();
                }
                break;
            }
            let kind = u32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]);
            if kind != message::MSG_PACKET {
                self.buffer.clear();
                return Err(TetherError::protocol(
                    "RNDIS data channel contained a non-packet message",
                ));
            }
            let length = u32::from_le_bytes([
                self.buffer[4],
                self.buffer[5],
                self.buffer[6],
                self.buffer[7],
            ]) as usize;
            if length < PACKET_HEADER_LEN || length > self.max_buffer {
                self.buffer.clear();
                return Err(TetherError::protocol(
                    "invalid RNDIS packet length while reassembling",
                ));
            }
            if self.buffer.len() < length {
                break;
            }
            let packet = message::parse_packet(&self.buffer[..length])?;
            if packet.data_length > 0 {
                frames.push(packet.data.to_vec());
            }
            self.buffer.drain(..length);
            // A packet message is followed by alignment padding only when
            // another message exists.  Accept zero padding up to alignment;
            // a real header starts with 0x01 and is never all zero.
            let mut zeros = 0;
            while zeros < 3
                && self.buffer.get(zeros).copied() == Some(0)
                && self.buffer.len() > zeros + 1
            {
                zeros += 1;
            }
            // Do not strip here: the next message may start immediately and
            // its first byte is 1.  Trailing zeros are handled by the
            // short-buffer branch above or by parse_header on the next loop.
            if frames.len() > 1024 {
                self.buffer.clear();
                return Err(TetherError::protocol(
                    "too many RNDIS frames in one reassembly batch",
                ));
            }
        }
        Ok(frames)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_data_offset_is_36() {
        let frame = vec![0u8; 60];
        let message = encode_packet_message(&frame, 4).unwrap();
        assert_eq!(&message[4..8], &104u32.to_le_bytes());
        assert_eq!(&message[8..12], &36u32.to_le_bytes());
        assert_eq!(&message[12..16], &60u32.to_le_bytes());
        assert_eq!(message.len(), 104);
    }

    #[test]
    fn pads_message_to_requested_alignment() {
        let frame = vec![0u8; 61];
        let message = encode_packet_message(&frame, 8).unwrap();
        assert_eq!(message.len(), 112); // 44 + 61 = 105, rounded to 112
        assert_eq!(&message[4..8], &112u32.to_le_bytes());
    }

    #[test]
    fn aggregates_respects_packet_and_byte_limits() {
        let a = vec![0u8; 60];
        let b = vec![0u8; 60];
        let c = vec![0u8; 60];
        let frames: Vec<&[u8]> = vec![&a, &b, &c];
        let encoded = encode_transfer(&frames, 2, 1024, 4, 512).unwrap();
        assert_eq!(encoded.frames, 2);
        assert_eq!(encoded.bytes.len(), 208);
        let reassembler = {
            let mut r = PacketReassembler::new(4096);
            r.push(&encoded.bytes).unwrap()
        };
        assert_eq!(reassembler.len(), 2);
        assert_eq!(reassembler[0], a);
    }

    #[test]
    fn adds_short_packet_pad_when_multiple_of_max_packet() {
        // 44 + 468 = 512 exactly.
        let frame = vec![0u8; 468];
        let encoded = encode_transfer(&[&frame], 1, 4096, 4, 512).unwrap();
        assert_eq!(encoded.bytes.len(), 513);
        assert_eq!(encoded.bytes[512], 0);
        let mut reassembler = PacketReassembler::new(4096);
        let frames = reassembler.push(&encoded.bytes).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), 468);
    }

    #[test]
    fn rejects_frame_larger_than_transfer() {
        let frame = vec![0u8; 1500];
        let error = encode_transfer(&[&frame], 1, 1024, 4, 512).unwrap_err();
        assert_eq!(error.category.as_str(), "protocol_violation");
    }

    #[test]
    fn rejects_non_packet_message_on_data_channel() {
        let mut reassembler = PacketReassembler::new(4096);
        let mut bytes = vec![0u8; 44];
        bytes[0..4].copy_from_slice(&message::MSG_KEEPALIVE.to_le_bytes());
        bytes[4..8].copy_from_slice(&44u32.to_le_bytes());
        assert!(reassembler.push(&bytes).is_err());
    }
    #[test]
    fn reassembles_messages_split_at_every_boundary() {
        let a = vec![0x11u8; 60];
        let b = vec![0x22u8; 61];
        let encoded = encode_transfer(&[&a, &b], 2, 4096, 4, 512).unwrap();
        for split in 1..encoded.bytes.len() {
            let mut reassembler = PacketReassembler::new(4096);
            let mut frames = reassembler.push(&encoded.bytes[..split]).unwrap();
            frames.extend(reassembler.push(&encoded.bytes[split..]).unwrap());
            assert_eq!(frames, vec![a.clone(), b.clone()], "split at {split}");
        }
    }

    #[test]
    fn reassembler_accepts_trailing_short_packet_pad() {
        let frame = vec![0x33u8; 468];
        let encoded = encode_transfer(&[&frame], 1, 4096, 4, 512).unwrap();
        let mut reassembler = PacketReassembler::new(4096);
        let frames = reassembler.push(&encoded.bytes).unwrap();
        assert_eq!(frames, vec![frame]);
        assert!(reassembler.push(&[]).unwrap().is_empty());
    }
}
