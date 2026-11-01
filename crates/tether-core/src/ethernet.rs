//! Ethernet frame validation and small, independent DHCP/ARP fixtures.
//!
//! The bridge treats frames as opaque bytes, but the protocol core must reject
//! truncated or oversized frames and the synthetic tests need a *real*
//! Ethernet/IPv4/UDP/DHCP exchange instead of an arbitrary byte blob.

use crate::error::{Result, TetherError};

pub const ETHERNET_HEADER_LEN: usize = 14;
pub const ETHERNET_MIN_FRAME: usize = ETHERNET_HEADER_LEN;
/// Maximum frame without FCS, including one optional 802.1Q tag.
pub const ETHERNET_MAX_FRAME: usize = 1518;
pub const ETHERTYPE_IPV4: u16 = 0x0800;
pub const ETHERTYPE_ARP: u16 = 0x0806;
pub const ETHERTYPE_IPV6: u16 = 0x86dd;
pub const ETHERTYPE_VLAN: u16 = 0x8100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MacAddr(pub [u8; 6]);

impl MacAddr {
    pub const BROADCAST: Self = Self([0xff; 6]);
    pub const ZERO: Self = Self([0; 6]);

    pub fn is_broadcast(self) -> bool {
        self.0 == [0xff; 6]
    }

    pub fn is_multicast(self) -> bool {
        self.0[0] & 0x01 != 0
    }

    pub fn is_unicast(self) -> bool {
        !self.is_multicast()
    }

    pub fn as_bytes(self) -> [u8; 6] {
        self.0
    }
}

impl std::fmt::Display for MacAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.0[0], self.0[1], self.0[2], self.0[3], self.0[4], self.0[5]
        )
    }
}

pub fn parse_mac(text: &str) -> Result<MacAddr> {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    if parts.len() != 6 {
        return Err(TetherError::new(
            crate::error::ErrorCategory::Configuration,
            "MAC address must contain six octets separated by ':' or '-'",
        ));
    }
    let mut mac = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        if part.len() != 2 {
            return Err(TetherError::new(
                crate::error::ErrorCategory::Configuration,
                "each MAC octet must have two hex digits",
            ));
        }
        mac[index] = u8::from_str_radix(part, 16).map_err(|_| {
            TetherError::new(
                crate::error::ErrorCategory::Configuration,
                "invalid hexadecimal MAC address",
            )
        })?;
    }
    Ok(MacAddr(mac))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EthernetFrame<'a> {
    pub destination: MacAddr,
    pub source: MacAddr,
    pub ethertype: u16,
    /// Payload after the Ethernet header (and after a VLAN tag, if present).
    pub payload: &'a [u8],
    pub vlan_id: Option<u16>,
}

pub fn parse_ethernet(frame: &[u8]) -> Result<EthernetFrame<'_>> {
    if frame.len() < ETHERNET_MIN_FRAME {
        return Err(TetherError::protocol(
            "Ethernet frame shorter than 14 bytes",
        ));
    }
    if frame.len() > ETHERNET_MAX_FRAME {
        return Err(TetherError::protocol("Ethernet frame exceeds 1518 bytes"));
    }
    let mut destination = [0u8; 6];
    destination.copy_from_slice(&frame[0..6]);
    let mut source = [0u8; 6];
    source.copy_from_slice(&frame[6..12]);
    let mut ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let mut payload_start = ETHERNET_HEADER_LEN;
    let mut vlan_id = None;
    if ethertype == ETHERTYPE_VLAN {
        if frame.len() < 18 {
            return Err(TetherError::protocol("truncated 802.1Q Ethernet frame"));
        }
        vlan_id = Some(u16::from_be_bytes([frame[14], frame[15]]) & 0x0fff);
        ethertype = u16::from_be_bytes([frame[16], frame[17]]);
        payload_start = 18;
    }
    Ok(EthernetFrame {
        destination: MacAddr(destination),
        source: MacAddr(source),
        ethertype,
        payload: &frame[payload_start..],
        vlan_id,
    })
}

/// Minimal ARP request with a zero target hardware address.  Constructed from
/// RFC 826 field definitions, not from any implementation.
pub fn build_arp_request(sender_mac: MacAddr, sender_ip: [u8; 4], target_ip: [u8; 4]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(42);
    frame.extend_from_slice(&MacAddr::BROADCAST.0);
    frame.extend_from_slice(&sender_mac.0);
    frame.extend_from_slice(&ETHERTYPE_ARP.to_be_bytes());
    frame.extend_from_slice(&1u16.to_be_bytes()); // hardware type: Ethernet
    frame.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes()); // protocol type
    frame.push(6); // hardware address length
    frame.push(4); // protocol address length
    frame.extend_from_slice(&1u16.to_be_bytes()); // opcode: request
    frame.extend_from_slice(&sender_mac.0);
    frame.extend_from_slice(&sender_ip);
    frame.extend_from_slice(&[0u8; 6]);
    frame.extend_from_slice(&target_ip);
    frame
}

/// Minimal DHCP DISCOVER with an Ethernet/IPv4/UDP envelope.  The IPv4 header
/// checksum is calculated; the UDP checksum is zero, which is legal for IPv4.
pub fn build_dhcp_discover(client_mac: MacAddr, xid: u32) -> Vec<u8> {
    let mut bootp = vec![0u8; 236];
    bootp[0] = 1; // BOOTREQUEST
    bootp[1] = 1; // Ethernet
    bootp[2] = 6; // hardware address length
    bootp[4..8].copy_from_slice(&xid.to_be_bytes());
    bootp[28..34].copy_from_slice(&client_mac.0);
    bootp.resize(240, 0);
    bootp[236..240].copy_from_slice(&[99, 130, 83, 99]); // magic cookie
    bootp.push(53); // DHCP message type
    bootp.push(1);
    bootp.push(1); // DISCOVER
    bootp.push(255); // end
    while bootp.len() < 300 {
        bootp.push(0);
    }

    let udp_len = 8 + bootp.len();
    let mut udp = Vec::with_capacity(udp_len);
    udp.extend_from_slice(&68u16.to_be_bytes()); // client port
    udp.extend_from_slice(&67u16.to_be_bytes()); // server port
    udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]); // checksum optional for IPv4
    udp.extend_from_slice(&bootp);

    let total_len = 20 + udp.len();
    let mut ip = Vec::with_capacity(total_len);
    ip.push(0x45); // IPv4, IHL 5
    ip.push(0); // DSCP/ECN
    ip.extend_from_slice(&(total_len as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 0]); // identification
    ip.extend_from_slice(&[0, 0]); // flags/fragment offset
    ip.push(64); // TTL
    ip.push(17); // UDP
    ip.extend_from_slice(&[0, 0]); // checksum placeholder
    ip.extend_from_slice(&[0, 0, 0, 0]); // source 0.0.0.0
    ip.extend_from_slice(&[255, 255, 255, 255]); // destination
    let checksum = internet_checksum(&ip);
    ip[10..12].copy_from_slice(&checksum.to_be_bytes());
    ip.extend_from_slice(&udp);

    let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + ip.len());
    frame.extend_from_slice(&MacAddr::BROADCAST.0);
    frame.extend_from_slice(&client_mac.0);
    frame.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    frame.extend_from_slice(&ip);
    frame
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpDiscover<'a> {
    pub xid: u32,
    pub client_mac: MacAddr,
    pub message_type: u8,
    pub raw_bootp: &'a [u8],
}

/// Minimal DHCP OFFER from the phone-side DHCP server.  Field layouts are
/// taken from RFC 2131/2132; the IPv4 header checksum is calculated.
pub fn build_dhcp_offer(
    client_mac: MacAddr,
    xid: u32,
    offered_ip: [u8; 4],
    server_ip: [u8; 4],
    server_mac: MacAddr,
) -> Vec<u8> {
    let mut bootp = vec![0u8; 240];
    bootp[0] = 2; // BOOTREPLY
    bootp[1] = 1; // Ethernet
    bootp[2] = 6; // hardware address length
    bootp[4..8].copy_from_slice(&xid.to_be_bytes());
    bootp[16..20].copy_from_slice(&offered_ip); // yiaddr
    bootp[28..34].copy_from_slice(&client_mac.0);
    bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
    bootp.extend_from_slice(&[53, 1, 2]); // DHCP OFFER
    bootp.extend_from_slice(&[54, 4]);
    bootp.extend_from_slice(&server_ip); // server identifier
    bootp.extend_from_slice(&[51, 4, 0, 0, 0x0e, 0x10]); // 3600 s lease
    bootp.extend_from_slice(&[1, 4, 255, 255, 255, 0]); // subnet mask
    bootp.extend_from_slice(&[3, 4]);
    bootp.extend_from_slice(&server_ip); // router
    bootp.extend_from_slice(&[6, 4]);
    bootp.extend_from_slice(&server_ip); // DNS
    bootp.push(255);
    while bootp.len() < 300 {
        bootp.push(0);
    }

    let udp_len = 8 + bootp.len();
    let mut udp = Vec::with_capacity(udp_len);
    udp.extend_from_slice(&67u16.to_be_bytes());
    udp.extend_from_slice(&68u16.to_be_bytes());
    udp.extend_from_slice(&(udp_len as u16).to_be_bytes());
    udp.extend_from_slice(&[0, 0]);
    udp.extend_from_slice(&bootp);

    let total_len = 20 + udp.len();
    let mut ip = Vec::with_capacity(total_len);
    ip.push(0x45);
    ip.push(0);
    ip.extend_from_slice(&(total_len as u16).to_be_bytes());
    ip.extend_from_slice(&[0, 0]);
    ip.extend_from_slice(&[0, 0]);
    ip.push(64);
    ip.push(17);
    ip.extend_from_slice(&[0, 0]);
    ip.extend_from_slice(&server_ip);
    ip.extend_from_slice(&[255, 255, 255, 255]);
    let checksum = internet_checksum(&ip);
    ip[10..12].copy_from_slice(&checksum.to_be_bytes());
    ip.extend_from_slice(&udp);

    let mut frame = Vec::with_capacity(ETHERNET_HEADER_LEN + ip.len());
    frame.extend_from_slice(&client_mac.0);
    frame.extend_from_slice(&server_mac.0);
    frame.extend_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
    frame.extend_from_slice(&ip);
    frame
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DhcpPacket<'a> {
    pub op: u8,
    pub xid: u32,
    pub client_mac: MacAddr,
    pub offered_ip: [u8; 4],
    pub message_type: u8,
    pub src_port: u16,
    pub dst_port: u16,
    pub raw_bootp: &'a [u8],
}

/// Parse a DHCP client or server packet sufficiently to validate the synthetic
/// exchange.  Malformed lengths and missing magic cookies are rejected.
pub fn parse_dhcp_packet(frame: &[u8]) -> Result<DhcpPacket<'_>> {
    let ethernet = parse_ethernet(frame)?;
    if ethernet.ethertype != ETHERTYPE_IPV4 {
        return Err(TetherError::protocol("not an IPv4 frame"));
    }
    let ip = ethernet.payload;
    if ip.len() < 20 || ip[0] >> 4 != 4 {
        return Err(TetherError::protocol("not an IPv4 packet"));
    }
    let ihl = ((ip[0] & 0x0f) as usize) * 4;
    if ihl < 20 || ip.len() < ihl + 8 {
        return Err(TetherError::protocol("truncated IPv4/UDP packet"));
    }
    if ip[9] != 17 {
        return Err(TetherError::protocol("not a UDP packet"));
    }
    let udp = &ip[ihl..];
    let src_port = u16::from_be_bytes([udp[0], udp[1]]);
    let dst_port = u16::from_be_bytes([udp[2], udp[3]]);
    let udp_len = u16::from_be_bytes([udp[4], udp[5]]) as usize;
    if udp_len < 8 + 240 || udp.len() < udp_len {
        return Err(TetherError::protocol("truncated DHCP message"));
    }
    let bootp = &udp[8..udp_len];
    if bootp.len() < 240 || bootp[236..240] != [99, 130, 83, 99] {
        return Err(TetherError::protocol("missing DHCP magic cookie"));
    }
    let mut xid_bytes = [0u8; 4];
    xid_bytes.copy_from_slice(&bootp[4..8]);
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&bootp[28..34]);
    let mut offered_ip = [0u8; 4];
    offered_ip.copy_from_slice(&bootp[16..20]);
    let message_type = find_dhcp_option(bootp, 53)
        .and_then(|value| value.first().copied())
        .unwrap_or(0);
    Ok(DhcpPacket {
        op: bootp[0],
        xid: u32::from_be_bytes(xid_bytes),
        client_mac: MacAddr(mac),
        offered_ip,
        message_type,
        src_port,
        dst_port,
        raw_bootp: bootp,
    })
}

/// Parse a DHCP client DISCOVER.
pub fn parse_dhcp_discover(frame: &[u8]) -> Result<DhcpDiscover<'_>> {
    let packet = parse_dhcp_packet(frame)?;
    if packet.op != 1 || packet.src_port != 68 || packet.dst_port != 67 {
        return Err(TetherError::protocol("not a DHCP client packet"));
    }
    Ok(DhcpDiscover {
        xid: packet.xid,
        client_mac: packet.client_mac,
        message_type: packet.message_type,
        raw_bootp: packet.raw_bootp,
    })
}

pub fn find_dhcp_option(bootp: &[u8], option: u8) -> Option<&[u8]> {
    if bootp.len() < 240 {
        return None;
    }
    let mut offset = 240;
    while offset < bootp.len() {
        let code = bootp[offset];
        offset += 1;
        if code == 255 {
            return None;
        }
        if code == 0 {
            continue;
        }
        if offset >= bootp.len() {
            return None;
        }
        let len = bootp[offset] as usize;
        offset += 1;
        if offset + len > bootp.len() {
            return None;
        }
        if code == option {
            return Some(&bootp[offset..offset + len]);
        }
        offset += len;
    }
    None
}

pub fn internet_checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut chunks = bytes.chunks_exact(2);
    for chunk in &mut chunks {
        sum += u32::from(u16::from_be_bytes([chunk[0], chunk[1]]));
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += u32::from(u16::from_be_bytes([last, 0]));
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arp_request_is_structurally_valid() {
        let mac = MacAddr([0x02, 0, 0, 0, 0, 1]);
        let frame = build_arp_request(mac, [192, 168, 42, 2], [192, 168, 42, 1]);
        let parsed = parse_ethernet(&frame).unwrap();
        assert_eq!(parsed.ethertype, ETHERTYPE_ARP);
        assert!(parsed.destination.is_broadcast());
        assert_eq!(parsed.payload.len(), 28);
        assert_eq!(&parsed.payload[0..2], &[0, 1]);
        assert_eq!(&parsed.payload[2..4], &[0x08, 0x00]);
    }

    #[test]
    fn dhcp_discover_has_correct_envelope() {
        let mac = MacAddr([0x02, 0, 0, 0, 0, 2]);
        let frame = build_dhcp_discover(mac, 0x1234_5678);
        let parsed = parse_dhcp_discover(&frame).unwrap();
        assert_eq!(parsed.xid, 0x1234_5678);
        assert_eq!(parsed.client_mac, mac);
        assert_eq!(parsed.message_type, 1);
        let ip = parse_ethernet(&frame).unwrap().payload;
        assert_eq!(internet_checksum(&ip[..20]), 0);
    }

    #[test]
    fn rejects_short_frame() {
        assert!(parse_ethernet(&[0u8; 13]).is_err());
    }

    #[test]
    fn rejects_oversized_frame() {
        assert!(parse_ethernet(&vec![0u8; 1519]).is_err());
    }

    #[test]
    fn parses_vlan_frame() {
        let mut frame = vec![0u8; 18];
        frame[0..6].copy_from_slice(&[0xff; 6]);
        frame[6..12].copy_from_slice(&[0x02, 0, 0, 0, 0, 1]);
        frame[12..14].copy_from_slice(&ETHERTYPE_VLAN.to_be_bytes());
        frame[14..16].copy_from_slice(&[0x00, 0x2a]);
        frame[16..18].copy_from_slice(&ETHERTYPE_IPV4.to_be_bytes());
        let parsed = parse_ethernet(&frame).unwrap();
        assert_eq!(parsed.vlan_id, Some(42));
        assert_eq!(parsed.ethertype, ETHERTYPE_IPV4);
        assert!(parsed.payload.is_empty());
    }
    #[test]
    fn dhcp_offer_has_server_fields_and_valid_checksum() {
        let client = MacAddr([0x02, 0, 0, 0, 0, 0x42]);
        let server = MacAddr([0x02, 0, 0, 0, 0, 0x11]);
        let frame = build_dhcp_offer(
            client,
            0x1234_5678,
            [192, 168, 42, 2],
            [192, 168, 42, 1],
            server,
        );
        let packet = parse_dhcp_packet(&frame).unwrap();
        assert_eq!(packet.op, 2);
        assert_eq!(packet.message_type, 2);
        assert_eq!(packet.src_port, 67);
        assert_eq!(packet.dst_port, 68);
        assert_eq!(packet.offered_ip, [192, 168, 42, 2]);
        assert_eq!(packet.client_mac, client);
        assert_eq!(packet.xid, 0x1234_5678);
        let ip = parse_ethernet(&frame).unwrap().payload;
        assert_eq!(internet_checksum(&ip[..20]), 0);
        assert_eq!(
            find_dhcp_option(packet.raw_bootp, 54),
            Some(&[192, 168, 42, 1][..])
        );
    }
}
