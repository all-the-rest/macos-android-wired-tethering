//! RNDIS object identifiers, status values and packet-filter bits.
//!
//! Values are from [MS-RNDIS] section 2.2.5 / 4 and the Microsoft OID
//! documentation.  Names are intentionally spelled exactly as in the
//! specification so that logs can be correlated with the source.

pub const OID_GEN_SUPPORTED_LIST: u32 = 0x0001_0101;
pub const OID_GEN_HARDWARE_STATUS: u32 = 0x0001_0102;
pub const OID_GEN_MEDIA_SUPPORTED: u32 = 0x0001_0103;
pub const OID_GEN_MEDIA_IN_USE: u32 = 0x0001_0104;
pub const OID_GEN_MAXIMUM_LOOKAHEAD: u32 = 0x0001_0105;
pub const OID_GEN_MAXIMUM_FRAME_SIZE: u32 = 0x0001_0106;
pub const OID_GEN_LINK_SPEED: u32 = 0x0001_0107;
pub const OID_GEN_TRANSMIT_BUFFER_SPACE: u32 = 0x0001_0108;
pub const OID_GEN_RECEIVE_BUFFER_SPACE: u32 = 0x0001_0109;
pub const OID_GEN_TRANSMIT_BLOCK_SIZE: u32 = 0x0001_010a;
pub const OID_GEN_RECEIVE_BLOCK_SIZE: u32 = 0x0001_010b;
pub const OID_GEN_VENDOR_ID: u32 = 0x0001_010c;
pub const OID_GEN_VENDOR_DESCRIPTION: u32 = 0x0001_010d;
pub const OID_GEN_CURRENT_PACKET_FILTER: u32 = 0x0001_010e;
pub const OID_GEN_CURRENT_LOOKAHEAD: u32 = 0x0001_010f;
pub const OID_GEN_DRIVER_VERSION: u32 = 0x0001_0110;
pub const OID_GEN_MAXIMUM_TOTAL_SIZE: u32 = 0x0001_0111;
pub const OID_GEN_PROTOCOL_OPTIONS: u32 = 0x0001_0112;
pub const OID_GEN_MAC_OPTIONS: u32 = 0x0001_0113;
pub const OID_GEN_MEDIA_CONNECT_STATUS: u32 = 0x0001_0114;
pub const OID_GEN_MAXIMUM_SEND_PACKETS: u32 = 0x0001_0115;
pub const OID_GEN_MEDIA_CAPABILITIES: u32 = 0x0001_0201;
pub const OID_GEN_PHYSICAL_MEDIUM: u32 = 0x0001_0202;

pub const OID_802_3_PERMANENT_ADDRESS: u32 = 0x0101_0101;
pub const OID_802_3_CURRENT_ADDRESS: u32 = 0x0101_0102;
pub const OID_802_3_MULTICAST_LIST: u32 = 0x0101_0103;
pub const OID_802_3_MAXIMUM_LIST_SIZE: u32 = 0x0101_0104;
pub const OID_802_3_MAC_OPTIONS: u32 = 0x0101_0105;

pub const PACKET_TYPE_DIRECTED: u32 = 0x0000_0001;
pub const PACKET_TYPE_MULTICAST: u32 = 0x0000_0002;
pub const PACKET_TYPE_ALL_MULTICAST: u32 = 0x0000_0004;
pub const PACKET_TYPE_BROADCAST: u32 = 0x0000_0008;
pub const PACKET_TYPE_SOURCE_ROUTING: u32 = 0x0000_0010;
pub const PACKET_TYPE_PROMISCUOUS: u32 = 0x0000_0020;
pub const PACKET_TYPE_SMT: u32 = 0x0000_0040;
pub const PACKET_TYPE_ALL_LOCAL: u32 = 0x0000_0080;
pub const PACKET_TYPE_GROUP: u32 = 0x0000_1000;
pub const PACKET_TYPE_ALL_FUNCTIONAL: u32 = 0x0000_2000;
pub const PACKET_TYPE_FUNCTIONAL: u32 = 0x0000_4000;
pub const PACKET_TYPE_MAC_FRAME: u32 = 0x0000_8000;

/// Linux's default filter; the most permissive reasonable 802.3 filter.
pub const DEFAULT_PACKET_FILTER: u32 = PACKET_TYPE_DIRECTED
    | PACKET_TYPE_BROADCAST
    | PACKET_TYPE_ALL_MULTICAST
    | PACKET_TYPE_PROMISCUOUS;

pub const MEDIA_CONNECTED: u32 = 0;
pub const MEDIA_DISCONNECTED: u32 = 1;

pub const STATUS_SUCCESS: u32 = 0x0000_0000;
pub const STATUS_PENDING: u32 = 0x0000_0103;
pub const STATUS_MEDIA_CONNECT: u32 = 0x4001_000b;
pub const STATUS_MEDIA_DISCONNECT: u32 = 0x4001_000c;
pub const STATUS_INVALID_DATA: u32 = 0xc001_0015;
pub const STATUS_INVALID_LENGTH: u32 = 0xc001_0014;
pub const STATUS_INVALID_OID: u32 = 0xc001_0017;
pub const STATUS_NOT_SUPPORTED: u32 = 0xc000_00bb;
pub const STATUS_FAILURE: u32 = 0xc000_0001;

pub fn status_name(status: u32) -> &'static str {
    match status {
        STATUS_SUCCESS => "STATUS_SUCCESS",
        STATUS_PENDING => "STATUS_PENDING",
        STATUS_MEDIA_CONNECT => "RNDIS_STATUS_MEDIA_CONNECT",
        STATUS_MEDIA_DISCONNECT => "RNDIS_STATUS_MEDIA_DISCONNECT",
        STATUS_INVALID_DATA => "RNDIS_STATUS_INVALID_DATA",
        STATUS_INVALID_LENGTH => "RNDIS_STATUS_INVALID_LENGTH",
        STATUS_INVALID_OID => "RNDIS_STATUS_INVALID_OID",
        STATUS_NOT_SUPPORTED => "RNDIS_STATUS_NOT_SUPPORTED",
        STATUS_FAILURE => "RNDIS_STATUS_FAILURE",
        0x0001_0001 => "RNDIS_STATUS_NOT_RECOGNIZED",
        0x0001_0002 => "RNDIS_STATUS_NOT_COPIED",
        0x0001_0003 => "RNDIS_STATUS_NOT_ACCEPTED",
        0x4001_0003 => "RNDIS_STATUS_ONLINE",
        0x4001_0004 => "RNDIS_STATUS_RESET_START",
        0x4001_0005 => "RNDIS_STATUS_RESET_END",
        0x4001_0013 => "RNDIS_STATUS_LINK_SPEED_CHANGE",
        _ => "RNDIS_STATUS_UNKNOWN",
    }
}

pub fn oid_name(oid: u32) -> &'static str {
    match oid {
        OID_GEN_SUPPORTED_LIST => "OID_GEN_SUPPORTED_LIST",
        OID_GEN_HARDWARE_STATUS => "OID_GEN_HARDWARE_STATUS",
        OID_GEN_MEDIA_SUPPORTED => "OID_GEN_MEDIA_SUPPORTED",
        OID_GEN_MEDIA_IN_USE => "OID_GEN_MEDIA_IN_USE",
        OID_GEN_MAXIMUM_FRAME_SIZE => "OID_GEN_MAXIMUM_FRAME_SIZE",
        OID_GEN_LINK_SPEED => "OID_GEN_LINK_SPEED",
        OID_GEN_CURRENT_PACKET_FILTER => "OID_GEN_CURRENT_PACKET_FILTER",
        OID_GEN_MAXIMUM_TOTAL_SIZE => "OID_GEN_MAXIMUM_TOTAL_SIZE",
        OID_GEN_MEDIA_CONNECT_STATUS => "OID_GEN_MEDIA_CONNECT_STATUS",
        OID_GEN_PHYSICAL_MEDIUM => "OID_GEN_PHYSICAL_MEDIUM",
        OID_802_3_PERMANENT_ADDRESS => "OID_802_3_PERMANENT_ADDRESS",
        OID_802_3_CURRENT_ADDRESS => "OID_802_3_CURRENT_ADDRESS",
        OID_802_3_MULTICAST_LIST => "OID_802_3_MULTICAST_LIST",
        OID_802_3_MAXIMUM_LIST_SIZE => "OID_802_3_MAXIMUM_LIST_SIZE",
        _ => "UNKNOWN_OID",
    }
}

pub fn parse_u32_le(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < 4 {
        return None;
    }
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub fn parse_mac(bytes: &[u8]) -> Option<[u8; 6]> {
    if bytes.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    mac.copy_from_slice(bytes);
    Some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oid_values_match_spec() {
        assert_eq!(OID_GEN_CURRENT_PACKET_FILTER, 0x0001_010e);
        assert_eq!(OID_802_3_PERMANENT_ADDRESS, 0x0101_0101);
        assert_eq!(OID_GEN_MEDIA_CONNECT_STATUS, 0x0001_0114);
    }

    #[test]
    fn default_filter_has_directed_broadcast_multicast() {
        assert_ne!(DEFAULT_PACKET_FILTER & PACKET_TYPE_DIRECTED, 0);
        assert_ne!(DEFAULT_PACKET_FILTER & PACKET_TYPE_BROADCAST, 0);
        assert_ne!(DEFAULT_PACKET_FILTER & PACKET_TYPE_ALL_MULTICAST, 0);
    }
}
