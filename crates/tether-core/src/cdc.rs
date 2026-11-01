//! CDC class-specific descriptor parsing.
//!
//! Only fields needed for classification and diagnostics are decoded.  Every
//! length is checked before use; unknown subtypes are retained as raw bytes so
//! that a future protocol implementation does not need to change the parser.

use crate::error::{Result, TetherError};

pub const CS_INTERFACE: u8 = 0x24;
pub const CS_ENDPOINT: u8 = 0x25;

pub const SUBTYPE_HEADER: u8 = 0x00;
pub const SUBTYPE_CALL_MANAGEMENT: u8 = 0x01;
pub const SUBTYPE_ACM: u8 = 0x02;
pub const SUBTYPE_UNION: u8 = 0x06;
pub const SUBTYPE_ETHERNET: u8 = 0x0f;
pub const SUBTYPE_NCM: u8 = 0x1a;
pub const SUBTYPE_MBIM: u8 = 0x1b;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdcUnion {
    pub control_interface: u8,
    pub subordinates: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdcEthernet {
    pub mac_string_index: u8,
    pub max_segment_size: u16,
    pub number_mc_filters: u16,
    pub number_power_filters: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdcNcm {
    pub version_bcd: u16,
    pub network_capabilities: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionalDescriptor {
    Header { bcd_cdc: u16 },
    Union(CdcUnion),
    Ethernet(CdcEthernet),
    Ncm(CdcNcm),
    Unknown { subtype: u8, bytes: Vec<u8> },
}

/// Parse the class-specific interface descriptor block that follows an
/// interface descriptor.  A malformed descriptor aborts parsing: silently
/// guessing at a truncated union would defeat the validation requirement.
pub fn parse_interface_extra(bytes: &[u8]) -> Result<Vec<FunctionalDescriptor>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes.len() - offset < 2 {
            return Err(TetherError::descriptor(
                "truncated CDC functional descriptor header",
            ));
        }
        let len = bytes[offset] as usize;
        let kind = bytes[offset + 1];
        if len < 2 {
            return Err(TetherError::descriptor(
                "CDC functional descriptor has a zero/short length",
            ));
        }
        if offset + len > bytes.len() {
            return Err(TetherError::descriptor(
                "CDC functional descriptor exceeds its interface extra block",
            ));
        }
        let body = &bytes[offset..offset + len];
        if kind == CS_INTERFACE {
            if len < 3 {
                return Err(TetherError::descriptor(
                    "CDC functional descriptor has no subtype",
                ));
            }
            let subtype = body[2];
            let decoded = match subtype {
                SUBTYPE_HEADER => {
                    if len < 5 {
                        return Err(TetherError::descriptor("short CDC header descriptor"));
                    }
                    FunctionalDescriptor::Header {
                        bcd_cdc: le_u16(&body[3..5]),
                    }
                }
                SUBTYPE_UNION => {
                    if len < 5 {
                        return Err(TetherError::descriptor("short CDC union descriptor"));
                    }
                    FunctionalDescriptor::Union(CdcUnion {
                        control_interface: body[3],
                        subordinates: body[4..].to_vec(),
                    })
                }
                SUBTYPE_ETHERNET => {
                    if len < 13 {
                        return Err(TetherError::descriptor(
                            "short CDC Ethernet functional descriptor",
                        ));
                    }
                    FunctionalDescriptor::Ethernet(CdcEthernet {
                        mac_string_index: body[3],
                        max_segment_size: le_u16(&body[8..10]),
                        number_mc_filters: le_u16(&body[10..12]),
                        number_power_filters: body[12],
                    })
                }
                SUBTYPE_NCM => {
                    if len < 6 {
                        return Err(TetherError::descriptor("short CDC NCM descriptor"));
                    }
                    FunctionalDescriptor::Ncm(CdcNcm {
                        version_bcd: le_u16(&body[3..5]),
                        network_capabilities: body[5],
                    })
                }
                other => FunctionalDescriptor::Unknown {
                    subtype: other,
                    bytes: body[3..].to_vec(),
                },
            };
            out.push(decoded);
        }
        offset += len;
    }
    Ok(out)
}

/// Collect only Interface Association Descriptors from a raw extra block.
/// libusb places IADs in the configuration's extra area on macOS.
pub fn parse_iads(bytes: &[u8]) -> Result<Vec<super::descriptor::InterfaceAssociation>> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        if bytes.len() - offset < 2 {
            return Err(TetherError::descriptor(
                "truncated descriptor in extra block",
            ));
        }
        let len = bytes[offset] as usize;
        if len < 2 {
            return Err(TetherError::descriptor(
                "zero-length descriptor in extra block",
            ));
        }
        if offset + len > bytes.len() {
            return Err(TetherError::descriptor("descriptor exceeds extra block"));
        }
        if bytes[offset + 1] == 0x0b {
            if len < 8 {
                return Err(TetherError::descriptor(
                    "short interface association descriptor",
                ));
            }
            out.push(super::descriptor::InterfaceAssociation {
                first_interface: bytes[offset + 2],
                interface_count: bytes[offset + 3],
                function_class: bytes[offset + 4],
                function_subclass: bytes[offset + 5],
                function_protocol: bytes[offset + 6],
                function_string_index: bytes[offset + 7],
            });
        }
        offset += len;
    }
    Ok(out)
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}
