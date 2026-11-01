//! USB descriptor parsing and protocol classification.
//!
//! The parser is intentionally independent of libusb and operating system
//! APIs.  It consumes a raw configuration descriptor, validates every length
//! and offset, and produces a normalised representation.  The libusb adapter
//! builds the same normalised representation from libusb's decoded
//! descriptors.
//!
//! Descriptor values are from the USB 2.0/3.x specification, USB-IF CDC class
//! definitions and [MS-RNDIS].  Device class matching deliberately uses the
//! class/subclass/protocol tuple plus CDC union/IAD information, not a vendor
//! or product allow-list.

use crate::cdc::{self, CdcEthernet, CdcNcm, CdcUnion, FunctionalDescriptor};
use crate::error::{Result, TetherError};

pub const USB_DT_DEVICE: u8 = 0x01;
pub const USB_DT_CONFIG: u8 = 0x02;
pub const USB_DT_INTERFACE: u8 = 0x04;
pub const USB_DT_ENDPOINT: u8 = 0x05;
pub const USB_DT_IAD: u8 = 0x0b;

pub const USB_CLASS_COMM: u8 = 0x02;
pub const USB_CLASS_DATA: u8 = 0x0a;
pub const USB_CLASS_MISC: u8 = 0xef;
pub const USB_CLASS_WIRELESS: u8 = 0xe0;

pub const CDC_SUBCLASS_ETHERNET: u8 = 0x06;
pub const CDC_SUBCLASS_NCM: u8 = 0x0d;
pub const CDC_PROTOCOL_NONE: u8 = 0x00;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub usb_version_bcd: u16,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub max_packet_size_0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub device_version_bcd: u16,
    pub num_configurations: u8,
}

pub fn parse_device_descriptor(bytes: &[u8]) -> Result<DeviceDescriptor> {
    if bytes.len() < 18 {
        return Err(TetherError::descriptor("truncated USB device descriptor"));
    }
    if bytes[0] < 18 || bytes[1] != USB_DT_DEVICE {
        return Err(TetherError::descriptor("not a USB device descriptor"));
    }
    Ok(DeviceDescriptor {
        usb_version_bcd: le_u16(&bytes[2..4]),
        class: bytes[4],
        subclass: bytes[5],
        protocol: bytes[6],
        max_packet_size_0: bytes[7],
        vendor_id: le_u16(&bytes[8..10]),
        product_id: le_u16(&bytes[10..12]),
        device_version_bcd: le_u16(&bytes[12..14]),
        num_configurations: bytes[17],
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub address: u8,
    pub transfer_type: TransferType,
    pub max_packet_size: u16,
    pub interval: u8,
    pub extra: Vec<u8>,
}

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }

    pub fn is_out(&self) -> bool {
        self.address & 0x80 == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AltSetting {
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub declared_endpoints: u8,
    pub endpoints: Vec<Endpoint>,
    pub extra: Vec<u8>,
}

impl AltSetting {
    pub fn bulk_pair(&self) -> Option<(&Endpoint, &Endpoint)> {
        let input = self.endpoints.iter().find(|ep| {
            ep.transfer_type == TransferType::Bulk && ep.is_in() && ep.max_packet_size > 0
        });
        let output = self.endpoints.iter().find(|ep| {
            ep.transfer_type == TransferType::Bulk && ep.is_out() && ep.max_packet_size > 0
        });
        match (input, output) {
            (Some(i), Some(o)) => Some((i, o)),
            _ => None,
        }
    }

    pub fn interrupt_in(&self) -> Option<&Endpoint> {
        self.endpoints.iter().find(|ep| {
            ep.transfer_type == TransferType::Interrupt && ep.is_in() && ep.max_packet_size > 0
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbInterface {
    pub number: u8,
    pub class_specific: Vec<u8>,
    pub alt_settings: Vec<AltSetting>,
}

impl UsbInterface {
    pub fn alt(&self, alt: u8) -> Option<&AltSetting> {
        self.alt_settings.iter().find(|s| s.alt == alt)
    }

    pub fn preferred_control_alt(&self) -> Option<&AltSetting> {
        self.alt(0).or_else(|| self.alt_settings.first())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceAssociation {
    pub first_interface: u8,
    pub interface_count: u8,
    pub function_class: u8,
    pub function_subclass: u8,
    pub function_protocol: u8,
    pub function_string_index: u8,
}

impl InterfaceAssociation {
    pub fn contains(&self, interface: u8) -> bool {
        let first = self.first_interface as u16;
        let last = first + self.interface_count as u16;
        (interface as u16) >= first && (interface as u16) < last
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbConfiguration {
    pub value: u8,
    pub attributes: u8,
    pub max_power_ma: u16,
    pub total_length: u16,
    pub interfaces: Vec<UsbInterface>,
    pub iads: Vec<InterfaceAssociation>,
    pub extra: Vec<u8>,
}

impl UsbConfiguration {
    pub fn find_interface(&self, number: u8) -> Option<&UsbInterface> {
        self.interfaces.iter().find(|i| i.number == number)
    }

    pub fn iad_for(&self, interface: u8) -> Option<&InterfaceAssociation> {
        self.iads.iter().find(|iad| iad.contains(interface))
    }

    /// Build a configuration from already-decoded parts.  The libusb adapter
    /// uses this to avoid re-encoding descriptors just to parse them again.
    pub fn from_parts(
        value: u8,
        attributes: u8,
        max_power_ma: u16,
        interfaces: Vec<UsbInterface>,
        iads: Vec<InterfaceAssociation>,
        extra: Vec<u8>,
    ) -> Result<Self> {
        let total_length = interfaces
            .iter()
            .map(|i| {
                9u16 + i
                    .alt_settings
                    .iter()
                    .map(|a| 9u16 + a.endpoints.len() as u16 * 7)
                    .sum::<u16>()
            })
            .sum();
        let cfg = Self {
            value,
            attributes,
            max_power_ma,
            total_length,
            interfaces,
            iads,
            extra,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let mut seen = Vec::new();
        for interface in &self.interfaces {
            if interface.alt_settings.is_empty() {
                return Err(TetherError::descriptor(
                    "interface without alternate setting",
                ));
            }
            if interface.class_specific.len() > 4096 {
                return Err(TetherError::descriptor(
                    "oversized class-specific descriptor block",
                ));
            }
            for alt in &interface.alt_settings {
                let key = (interface.number, alt.alt);
                if seen.contains(&key) {
                    return Err(TetherError::descriptor(
                        "duplicate interface/alternate-setting pair",
                    ));
                }
                seen.push(key);
                if alt.declared_endpoints as usize != alt.endpoints.len() {
                    return Err(TetherError::descriptor(
                        "endpoint count does not match the interface descriptor",
                    ));
                }
                let mut addresses = Vec::new();
                for ep in &alt.endpoints {
                    if ep.max_packet_size == 0 {
                        return Err(TetherError::descriptor("zero-sized endpoint"));
                    }
                    if addresses.contains(&ep.address) {
                        return Err(TetherError::descriptor("duplicate endpoint address"));
                    }
                    addresses.push(ep.address);
                }
            }
        }
        if self.iads.len() > 32 {
            return Err(TetherError::descriptor("too many interface associations"));
        }
        for iad in &self.iads {
            if iad.interface_count == 0 {
                return Err(TetherError::descriptor("empty interface association"));
            }
            if iad.first_interface as u16 + iad.interface_count as u16 > 256 {
                return Err(TetherError::descriptor(
                    "interface association exceeds interface number space",
                ));
            }
        }
        Ok(())
    }
}

/// Parse one raw configuration descriptor.  `bytes` may contain trailing
/// data; only `wTotalLength` is consumed.
pub fn parse_configuration(bytes: &[u8]) -> Result<UsbConfiguration> {
    if bytes.len() < 9 {
        return Err(TetherError::descriptor(
            "truncated USB configuration descriptor",
        ));
    }
    if bytes[0] < 9 || bytes[1] != USB_DT_CONFIG {
        return Err(TetherError::descriptor(
            "not a USB configuration descriptor",
        ));
    }
    let total = le_u16(&bytes[2..4]) as usize;
    if total < 9 {
        return Err(TetherError::descriptor(
            "invalid configuration total length",
        ));
    }
    if total > bytes.len() {
        return Err(TetherError::descriptor(
            "configuration descriptor is truncated relative to wTotalLength",
        ));
    }
    let mut cfg = UsbConfiguration {
        value: bytes[5],
        attributes: bytes[7],
        max_power_ma: u16::from(bytes[8]) * 2,
        total_length: total as u16,
        interfaces: Vec::new(),
        iads: Vec::new(),
        extra: Vec::new(),
    };
    let mut offset = 0usize;
    let mut current_interface: Option<usize> = None;
    let mut current_alt: Option<usize> = None;
    let mut last_endpoint: Option<(usize, usize, usize)> = None;
    let mut config_seen = false;

    while offset < total {
        if total - offset < 2 {
            return Err(TetherError::descriptor("truncated descriptor header"));
        }
        let len = bytes[offset] as usize;
        if len < 2 {
            return Err(TetherError::descriptor("descriptor has an invalid length"));
        }
        if offset + len > total {
            return Err(TetherError::descriptor(
                "descriptor exceeds the configuration total length",
            ));
        }
        let kind = bytes[offset + 1];
        let body = &bytes[offset..offset + len];
        match kind {
            USB_DT_CONFIG => {
                if offset != 0 || config_seen {
                    return Err(TetherError::descriptor(
                        "configuration descriptor is not the first descriptor",
                    ));
                }
                config_seen = true;
            }
            USB_DT_INTERFACE => {
                if len < 9 {
                    return Err(TetherError::descriptor("short interface descriptor"));
                }
                let number = body[2];
                let alt = AltSetting {
                    alt: body[3],
                    declared_endpoints: body[4],
                    class: body[5],
                    subclass: body[6],
                    protocol: body[7],
                    endpoints: Vec::new(),
                    extra: Vec::new(),
                };
                let interface_index = if let Some(index) = cfg
                    .interfaces
                    .iter()
                    .position(|interface| interface.number == number)
                {
                    index
                } else {
                    cfg.interfaces.push(UsbInterface {
                        number,
                        class_specific: Vec::new(),
                        alt_settings: Vec::new(),
                    });
                    cfg.interfaces.len() - 1
                };
                let interface = &mut cfg.interfaces[interface_index];
                if interface.alt_settings.iter().any(|a| a.alt == alt.alt) {
                    return Err(TetherError::descriptor(
                        "duplicate interface alternate setting",
                    ));
                }
                interface.alt_settings.push(alt);
                current_interface = Some(interface_index);
                current_alt = Some(interface.alt_settings.len() - 1);
                last_endpoint = None;
            }
            USB_DT_ENDPOINT => {
                if len < 7 {
                    return Err(TetherError::descriptor("short endpoint descriptor"));
                }
                let (Some(interface_index), Some(alt_index)) = (current_interface, current_alt)
                else {
                    return Err(TetherError::descriptor(
                        "endpoint descriptor outside an interface",
                    ));
                };
                let attributes = body[3];
                let transfer_type = match attributes & 0x03 {
                    0 => TransferType::Control,
                    1 => TransferType::Isochronous,
                    2 => TransferType::Bulk,
                    _ => TransferType::Interrupt,
                };
                let endpoint = Endpoint {
                    address: body[2],
                    transfer_type,
                    max_packet_size: le_u16(&body[4..6]) & 0x07ff,
                    interval: body[6],
                    extra: Vec::new(),
                };
                let interface = &mut cfg.interfaces[interface_index];
                let alt_setting = &mut interface.alt_settings[alt_index];
                alt_setting.endpoints.push(endpoint);
                last_endpoint = Some((interface_index, alt_index, alt_setting.endpoints.len() - 1));
            }
            USB_DT_IAD => {
                if len < 8 {
                    return Err(TetherError::descriptor(
                        "short interface association descriptor",
                    ));
                }
                cfg.iads.push(InterfaceAssociation {
                    first_interface: body[2],
                    interface_count: body[3],
                    function_class: body[4],
                    function_subclass: body[5],
                    function_protocol: body[6],
                    function_string_index: body[7],
                });
            }
            cdc::CS_INTERFACE => {
                let Some(interface_index) = current_interface else {
                    return Err(TetherError::descriptor(
                        "class-specific interface descriptor outside an interface",
                    ));
                };
                cfg.interfaces[interface_index]
                    .class_specific
                    .extend_from_slice(body);
            }
            cdc::CS_ENDPOINT => {
                let Some((interface_index, alt_index, endpoint_index)) = last_endpoint else {
                    return Err(TetherError::descriptor(
                        "class-specific endpoint descriptor without an endpoint",
                    ));
                };
                cfg.interfaces[interface_index].alt_settings[alt_index].endpoints[endpoint_index]
                    .extra
                    .extend_from_slice(body);
            }
            _ => {
                if let Some((interface_index, alt_index, _)) = last_endpoint {
                    cfg.interfaces[interface_index].alt_settings[alt_index]
                        .extra
                        .extend_from_slice(body);
                } else {
                    cfg.extra.extend_from_slice(body);
                }
            }
        }
        offset += len;
    }
    if offset != total {
        return Err(TetherError::descriptor(
            "descriptor chain does not end at wTotalLength",
        ));
    }
    cfg.validate()?;
    Ok(cfg)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Rndis,
    Ncm,
    Ecm,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rndis => "RNDIS",
            Self::Ncm => "CDC-NCM",
            Self::Ecm => "CDC-ECM",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlInterface {
    pub number: u8,
    pub alt: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub interrupt_in: Option<Endpoint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataInterface {
    pub number: u8,
    pub alt: u8,
    pub bulk_in: Endpoint,
    pub bulk_out: Endpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TetherFunction {
    pub protocol: Protocol,
    pub configuration_value: u8,
    pub control: ControlInterface,
    pub data: DataInterface,
    pub union: Option<CdcUnion>,
    pub iad: Option<InterfaceAssociation>,
    pub ethernet: Option<CdcEthernet>,
    pub ncm: Option<CdcNcm>,
    pub functionals: Vec<FunctionalDescriptor>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportLevel {
    /// The protocol is implemented by this project.
    Usable,
    /// A protocol is present but should be handled by a native macOS driver.
    DelegateNative,
    /// The function was detected but is incomplete or unsafe to drive.
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedFunction {
    pub protocol: Protocol,
    pub support: SupportLevel,
    /// Present when the function was complete enough to describe.
    pub function: Option<TetherFunction>,
    pub reason: String,
}

impl DetectedFunction {
    pub fn usable(&self) -> Option<&TetherFunction> {
        match self.support {
            SupportLevel::Usable => self.function.as_ref(),
            _ => None,
        }
    }
}

fn is_rndis_control(class: u8, subclass: u8, protocol: u8) -> bool {
    matches!(
        (class, subclass, protocol),
        (USB_CLASS_WIRELESS, 0x01, 0x03)
            | (USB_CLASS_COMM, 0x02, 0xff)
            | (USB_CLASS_MISC, 0x04, 0x01)
            | (USB_CLASS_MISC, 0x01, 0x01)
    )
}

fn is_ncm_control(class: u8, subclass: u8, protocol: u8) -> bool {
    class == USB_CLASS_COMM && subclass == CDC_SUBCLASS_NCM && protocol == CDC_PROTOCOL_NONE
}

fn is_ecm_control(class: u8, subclass: u8, protocol: u8) -> bool {
    class == USB_CLASS_COMM && subclass == CDC_SUBCLASS_ETHERNET && protocol == CDC_PROTOCOL_NONE
}

fn protocol_for_control(class: u8, subclass: u8, protocol: u8) -> Option<Protocol> {
    if is_rndis_control(class, subclass, protocol) {
        Some(Protocol::Rndis)
    } else if is_ncm_control(class, subclass, protocol) {
        Some(Protocol::Ncm)
    } else if is_ecm_control(class, subclass, protocol) {
        Some(Protocol::Ecm)
    } else {
        None
    }
}

/// Classify all network functions in a configuration.  Ordering is stable:
/// interface number, then protocol.
pub fn classify_configuration(cfg: &UsbConfiguration) -> Vec<DetectedFunction> {
    let mut out: Vec<DetectedFunction> = Vec::new();

    for interface in &cfg.interfaces {
        let Some(control_alt) = interface.preferred_control_alt() else {
            continue;
        };
        let protocol = match protocol_for_control(
            control_alt.class,
            control_alt.subclass,
            control_alt.protocol,
        ) {
            Some(protocol) => protocol,
            None => {
                // Some Android devices put an IAD with the RNDIS function
                // class around a control interface whose own class is not one
                // of the known tuples.  Use the IAD as a secondary signal.
                if let Some(iad) = cfg.iad_for(interface.number) {
                    if is_rndis_control(
                        iad.function_class,
                        iad.function_subclass,
                        iad.function_protocol,
                    ) && (control_alt.interrupt_in().is_some()
                        || iad.first_interface == interface.number)
                    {
                        Protocol::Rndis
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }
        };

        if out.iter().any(|d| {
            d.function
                .as_ref()
                .is_some_and(|f| f.control.number == interface.number && f.protocol == protocol)
        }) {
            continue;
        }

        let mut notes = Vec::new();
        let functionals = match parse_control_functionals(interface, control_alt) {
            Ok(functionals) => functionals,
            Err(error) => {
                out.push(DetectedFunction {
                    protocol,
                    support: SupportLevel::Unsupported,
                    function: None,
                    reason: format!("invalid CDC functional descriptors: {}", error.message),
                });
                continue;
            }
        };
        let union = functionals.iter().find_map(|f| match f {
            FunctionalDescriptor::Union(union) => Some(union.clone()),
            _ => None,
        });
        let ethernet = functionals.iter().find_map(|f| match f {
            FunctionalDescriptor::Ethernet(eth) => Some(eth.clone()),
            _ => None,
        });
        let ncm = functionals.iter().find_map(|f| match f {
            FunctionalDescriptor::Ncm(ncm) => Some(ncm.clone()),
            _ => None,
        });

        if protocol == Protocol::Ncm && ncm.is_none() {
            notes.push("NCM class tuple without an NCM functional descriptor".to_string());
        }
        if protocol == Protocol::Ecm && ethernet.is_none() {
            notes.push("ECM class tuple without an Ethernet functional descriptor".to_string());
        }

        let iad = cfg.iad_for(interface.number).cloned();
        let data_interface = match find_data_interface(cfg, interface, union.as_ref(), iad.as_ref())
        {
            Ok(data) => data,
            Err(reason) => {
                out.push(DetectedFunction {
                    protocol,
                    support: SupportLevel::Unsupported,
                    function: None,
                    reason: reason.message,
                });
                continue;
            }
        };

        let control = ControlInterface {
            number: interface.number,
            alt: control_alt.alt,
            class: control_alt.class,
            subclass: control_alt.subclass,
            protocol: control_alt.protocol,
            interrupt_in: control_alt.interrupt_in().cloned(),
        };
        let function = TetherFunction {
            protocol,
            configuration_value: cfg.value,
            control,
            data: data_interface,
            union: union.clone(),
            iad,
            ethernet: ethernet.clone(),
            ncm: ncm.clone(),
            functionals,
            notes: notes.clone(),
        };

        let support = match protocol {
            Protocol::Rndis => SupportLevel::Usable,
            Protocol::Ncm | Protocol::Ecm => SupportLevel::DelegateNative,
        };
        let reason = match support {
            SupportLevel::Usable => "RNDIS implementation available".to_string(),
            SupportLevel::DelegateNative => {
                "macOS native CDC driver should own this interface".to_string()
            }
            SupportLevel::Unsupported => "unsupported".to_string(),
        };
        out.push(DetectedFunction {
            protocol,
            support,
            function: Some(function),
            reason,
        });
    }

    out.sort_by_key(|d| {
        let number = d
            .function
            .as_ref()
            .map(|f| f.control.number)
            .unwrap_or(u8::MAX);
        (number, d.protocol as u8)
    });
    out
}

fn parse_control_functionals(
    interface: &UsbInterface,
    control_alt: &AltSetting,
) -> Result<Vec<FunctionalDescriptor>> {
    let mut functionals = cdc::parse_interface_extra(&interface.class_specific)?;
    if !control_alt.extra.is_empty() {
        functionals.extend(cdc::parse_interface_extra(&control_alt.extra)?);
    }
    Ok(functionals)
}

fn find_data_interface(
    cfg: &UsbConfiguration,
    control: &UsbInterface,
    union: Option<&CdcUnion>,
    iad: Option<&InterfaceAssociation>,
) -> Result<DataInterface> {
    let mut candidates: Vec<u8> = Vec::new();
    if let Some(union) = union {
        for &number in &union.subordinates {
            if !candidates.contains(&number) {
                candidates.push(number);
            }
        }
        if union.control_interface != control.number {
            // Not fatal: some devices report a stale control number but a
            // correct subordinate list.  Record it via the caller's notes.
        }
    }
    if let Some(iad) = iad {
        for number in iad.first_interface..iad.first_interface.saturating_add(iad.interface_count) {
            if !candidates.contains(&number) && number != control.number {
                candidates.push(number);
            }
        }
    }
    // Final fallback: any CDC-Data interface that is not the control
    // interface.  This keeps detection working on devices with a missing or
    // malformed union descriptor.
    for interface in &cfg.interfaces {
        if interface.number != control.number && !candidates.contains(&interface.number) {
            if let Some(alt) = interface.preferred_control_alt() {
                if alt.class == USB_CLASS_DATA {
                    candidates.push(interface.number);
                }
            }
        }
    }

    let mut last_reason =
        String::from("no CDC-Data interface associated with the control interface");
    for number in candidates {
        let Some(interface) = cfg.find_interface(number) else {
            last_reason = format!("CDC union refers to missing interface {}", number);
            continue;
        };
        // Prefer a setting with both bulk endpoints; alt 0 first, then the
        // lowest alternate setting.
        let mut alts: Vec<&AltSetting> = interface.alt_settings.iter().collect();
        alts.sort_by_key(|a| a.alt);
        if let Some(alt) = alts.iter().find(|a| a.bulk_pair().is_some()) {
            let (bulk_in, bulk_out) = alt.bulk_pair().expect("bulk_pair checked above");
            return Ok(DataInterface {
                number: interface.number,
                alt: alt.alt,
                bulk_in: bulk_in.clone(),
                bulk_out: bulk_out.clone(),
            });
        }
        if let Some(alt) = alts.iter().find(|a| {
            a.class == USB_CLASS_DATA
                || a.endpoints
                    .iter()
                    .any(|e| e.transfer_type == TransferType::Bulk)
        }) {
            last_reason = format!(
                "data interface {} alternate setting {} has no bulk IN/OUT pair",
                interface.number, alt.alt
            );
        }
    }
    Err(TetherError::unsupported(last_reason))
}

/// Derive a stable, locally administered unicast MAC address from arbitrary
/// device key material.  This is used only when the phone does not report a
/// designated host address through OID_802_3_PERMANENT_ADDRESS.
pub fn host_mac_from_key(key: &[u8]) -> [u8; 6] {
    // FNV-1a is sufficient here: this is not a security boundary, only a
    // preference for a stable address per device.
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in key {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut mac = [0u8; 6];
    for (index, slot) in mac.iter_mut().enumerate() {
        *slot = ((hash >> (index * 8)) & 0xff) as u8;
    }
    mac[0] = (mac[0] | 0x02) & 0xfe;
    mac
}

pub fn valid_host_mac(mac: &[u8; 6]) -> bool {
    let unicast = mac[0] & 0x01 == 0;
    let locally_administered_or_global = true; // both are legal for a host NIC
    let not_zero = mac.iter().any(|&b| b != 0);
    unicast && locally_administered_or_global && not_zero
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_host_mac_is_local_unicast() {
        let a = host_mac_from_key(b"bus1-addr2-vid0x04e8");
        let b = host_mac_from_key(b"bus1-addr2-vid0x04e8");
        assert_eq!(a, b);
        assert_eq!(a[0] & 0x01, 0);
        assert_eq!(a[0] & 0x02, 0x02);
        assert!(valid_host_mac(&a));
    }

    #[test]
    fn host_mac_rejects_multicast_zero() {
        assert!(!valid_host_mac(&[0x01, 0, 0, 0, 0, 1]));
        assert!(!valid_host_mac(&[0, 0, 0, 0, 0, 0]));
    }
}
