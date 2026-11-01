//! USB device discovery and libusb descriptor adaptation.
//!
//! Discovery never claims an interface.  It reads descriptors, normalises them
//! into the hardware-independent model and classifies each function.  The
//! `list` and `diagnose` commands therefore work without root and without
//! disturbing ADB, MTP or a native macOS network driver.

use rusb::{Context, Device, DeviceHandle, UsbContext};
use tether_core::cdc::parse_iads;
use tether_core::descriptor::{
    classify_configuration, AltSetting, DetectedFunction, Endpoint, InterfaceAssociation,
    TransferType, UsbConfiguration, UsbInterface,
};
use tether_core::error::{ErrorCategory, Result, TetherError};

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub bus: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub speed: String,
    pub configurations: Vec<ConfigurationInfo>,
    pub descriptor_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ConfigurationInfo {
    pub value: u8,
    pub functions: Vec<DetectedFunction>,
}

impl DeviceInfo {
    pub fn display_name(&self) -> String {
        format!(
            "bus {:03} address {:03} {:04x}:{:04x}",
            self.bus, self.address, self.vendor_id, self.product_id
        )
    }

    pub fn usable_rndis(&self) -> Option<(&ConfigurationInfo, &DetectedFunction)> {
        for config in &self.configurations {
            for function in &config.functions {
                if function.usable().is_some() {
                    return Some((config, function));
                }
            }
        }
        None
    }

    pub fn detected_functions(&self) -> impl Iterator<Item = &DetectedFunction> {
        self.configurations
            .iter()
            .flat_map(|config| config.functions.iter())
    }
}

/// Enumerate all USB devices.  No device is opened.
pub fn list_devices() -> Result<Vec<DeviceInfo>> {
    let context = Context::new().map_err(map_rusb)?;
    let devices = context.devices().map_err(map_rusb)?;
    let mut out = Vec::new();
    for device in devices.iter() {
        out.push(inspect_device(&device));
    }
    out.sort_by_key(|device| (device.bus, device.address));
    Ok(out)
}

/// Inspect one USB device.  Descriptor errors are reported per device instead
/// of aborting the whole list; a broken composite device must not hide other
/// devices from diagnostics.
pub fn inspect_device(device: &Device<Context>) -> DeviceInfo {
    let descriptor = match device.device_descriptor() {
        Ok(descriptor) => descriptor,
        Err(error) => {
            return DeviceInfo {
                bus: device.bus_number(),
                address: device.address(),
                vendor_id: 0,
                product_id: 0,
                speed: speed_name(device.speed()),
                configurations: Vec::new(),
                descriptor_error: Some(error.to_string()),
            };
        }
    };
    let mut configurations = Vec::new();
    let mut descriptor_error = None;
    for index in 0..descriptor.num_configurations() {
        match device.config_descriptor(index) {
            Ok(config) => match normalize_configuration(device, index) {
                Ok(normalized) => {
                    let functions = classify_configuration(&normalized);
                    configurations.push(ConfigurationInfo {
                        value: config.number(),
                        functions,
                    });
                }
                Err(error) => {
                    descriptor_error = Some(error.message);
                }
            },
            Err(error) => {
                descriptor_error = Some(format!("configuration {index}: {error}"));
            }
        }
    }
    DeviceInfo {
        bus: device.bus_number(),
        address: device.address(),
        vendor_id: descriptor.vendor_id(),
        product_id: descriptor.product_id(),
        speed: speed_name(device.speed()),
        configurations,
        descriptor_error,
    }
}

fn normalize_configuration(device: &Device<Context>, index: u8) -> Result<UsbConfiguration> {
    let config = device
        .config_descriptor(index)
        .map_err(|error| TetherError::new(ErrorCategory::UsbTransfer, error.to_string()))?;
    let mut interfaces = Vec::new();
    for interface in config.interfaces() {
        let mut alt_settings = Vec::new();
        let mut class_specific = Vec::new();
        for descriptor in interface.descriptors() {
            let alt = descriptor.setting_number();
            let mut endpoints = Vec::new();
            for endpoint in descriptor.endpoint_descriptors() {
                endpoints.push(Endpoint {
                    address: endpoint.address(),
                    transfer_type: match endpoint.transfer_type() {
                        rusb::TransferType::Control => TransferType::Control,
                        rusb::TransferType::Isochronous => TransferType::Isochronous,
                        rusb::TransferType::Bulk => TransferType::Bulk,
                        rusb::TransferType::Interrupt => TransferType::Interrupt,
                    },
                    max_packet_size: endpoint.max_packet_size(),
                    interval: endpoint.interval(),
                    extra: endpoint.extra().unwrap_or(&[]).to_vec(),
                });
            }
            let extra = descriptor.extra().to_vec();
            if alt == 0 {
                class_specific.extend_from_slice(&extra);
            }
            alt_settings.push(AltSetting {
                alt,
                class: descriptor.class_code(),
                subclass: descriptor.sub_class_code(),
                protocol: descriptor.protocol_code(),
                declared_endpoints: descriptor.num_endpoints(),
                endpoints,
                extra: if alt == 0 { Vec::new() } else { extra },
            });
        }
        interfaces.push(UsbInterface {
            number: interface.number(),
            class_specific,
            alt_settings,
        });
    }

    let mut iads: Vec<InterfaceAssociation> = parse_iads(config.extra()).unwrap_or_default();
    for interface in config.interfaces() {
        for descriptor in interface.descriptors() {
            for iad in parse_iads(descriptor.extra()).unwrap_or_default() {
                if !iads.iter().any(|existing| {
                    existing.first_interface == iad.first_interface
                        && existing.interface_count == iad.interface_count
                        && existing.function_class == iad.function_class
                        && existing.function_subclass == iad.function_subclass
                        && existing.function_protocol == iad.function_protocol
                }) {
                    iads.push(iad);
                }
            }
        }
    }

    UsbConfiguration::from_parts(
        config.number(),
        attributes(config.self_powered(), config.remote_wakeup()),
        config.max_power(),
        interfaces,
        iads,
        config.extra().to_vec(),
    )
}

fn attributes(self_powered: bool, remote_wakeup: bool) -> u8 {
    let mut value = 0x80;
    if self_powered {
        value |= 0x40;
    }
    if remote_wakeup {
        value |= 0x20;
    }
    value
}

fn speed_name(speed: rusb::Speed) -> String {
    match speed {
        rusb::Speed::Low => "low".to_string(),
        rusb::Speed::Full => "full".to_string(),
        rusb::Speed::High => "high".to_string(),
        rusb::Speed::Super => "super".to_string(),
        rusb::Speed::SuperPlus => "super+".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Open a device and claim only the RNDIS control and data interfaces.
///
/// Existing native drivers are never detached.  If macOS (or another process)
/// already owns a required interface, the caller gets `UsbBusy` and can leave
/// the device to the native path.
pub struct OpenedDevice {
    pub handle: DeviceHandle<Context>,
    pub function: tether_core::descriptor::TetherFunction,
    pub bus: u8,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub claimed_interfaces: Vec<u8>,
}

pub fn open_rndis(bus: u8, address: u8) -> Result<OpenedDevice> {
    let context = Context::new().map_err(map_rusb)?;
    let devices = context.devices().map_err(map_rusb)?;
    let device = devices
        .iter()
        .find(|device| device.bus_number() == bus && device.address() == address)
        .ok_or_else(|| {
            TetherError::new(
                ErrorCategory::UsbNotFound,
                format!("USB device bus {bus} address {address} not found"),
            )
        })?;
    let descriptor = device.device_descriptor().map_err(map_rusb)?;
    let handle = device.open().map_err(map_rusb)?;
    let active = handle.active_configuration().map_err(map_rusb)?;

    let mut chosen: Option<tether_core::descriptor::TetherFunction> = None;
    let configuration_count = descriptor.num_configurations();
    for index in 0..configuration_count {
        let normalized = normalize_configuration(&device, index)?;
        if active != 0 && normalized.value != active {
            continue;
        }
        for detected in classify_configuration(&normalized) {
            if let Some(function) = detected.usable() {
                chosen = Some(function.clone());
                break;
            }
        }
        if chosen.is_some() {
            break;
        }
    }
    let function = chosen.ok_or_else(|| {
        TetherError::unsupported(
            "this device has no usable RNDIS function in its active configuration",
        )
    })?;

    if active == 0 {
        handle
            .set_active_configuration(function.configuration_value)
            .map_err(map_rusb)?;
    } else if active != function.configuration_value {
        return Err(TetherError::unsupported(format!(
            "device is in configuration {active}, RNDIS function is in configuration {}",
            function.configuration_value
        )));
    }

    let plan = claim_plan(&function);
    let mut claimed = Vec::new();
    for interface in &plan {
        if interface_in_use(&handle, *interface)? {
            for claimed_interface in &claimed {
                let _ = handle.release_interface(*claimed_interface);
            }
            return Err(TetherError::new(
                ErrorCategory::UsbBusy,
                format!(
                    "interface {interface} is claimed by a native/other driver; not detaching it"
                ),
            ));
        }
    }
    for interface in &plan {
        handle.claim_interface(*interface).map_err(|error| {
            for claimed_interface in &claimed {
                let _ = handle.release_interface(*claimed_interface);
            }
            map_rusb(error)
        })?;
        claimed.push(*interface);
    }
    if function.data.alt != 0 {
        if let Err(error) = handle.set_alternate_setting(function.data.number, function.data.alt) {
            for claimed_interface in &claimed {
                let _ = handle.release_interface(*claimed_interface);
            }
            return Err(TetherError::new(
                ErrorCategory::UsbTransfer,
                format!(
                    "cannot select alternate setting {} on interface {}: {}",
                    function.data.alt, function.data.number, error
                ),
            ));
        }
    }

    Ok(OpenedDevice {
        handle,
        function,
        bus,
        address,
        vendor_id: descriptor.vendor_id(),
        product_id: descriptor.product_id(),
        claimed_interfaces: claimed,
    })
}

/// Interfaces that an RNDIS session needs to claim.
///
/// Only the control interface and the data interface selected by the protocol
/// classification are touched.  ADB, MTP, charging and any other interface of
/// a composite device remain available to macOS and to other processes.
pub fn claim_plan(function: &tether_core::descriptor::TetherFunction) -> Vec<u8> {
    let mut interfaces = Vec::with_capacity(2);
    for interface in [function.control.number, function.data.number] {
        if !interfaces.contains(&interface) {
            interfaces.push(interface);
        }
    }
    interfaces
}

fn interface_in_use(handle: &DeviceHandle<Context>, interface: u8) -> Result<bool> {
    match handle.kernel_driver_active(interface) {
        Ok(active) => Ok(active),
        Err(rusb::Error::NotSupported) => Ok(false),
        Err(error) => Err(map_rusb(error)),
    }
}

/// Locate a device by a short user-selectable selector: `bus:address`,
/// `bus-address`, or decimal address when unique.
pub fn resolve_selector(selector: &str, devices: &[DeviceInfo]) -> Result<(u8, u8)> {
    let trimmed = selector.trim();
    let parts: Vec<&str> = trimmed.split([':', '-']).collect();
    if parts.len() == 2 {
        let bus = parts[0]
            .parse::<u8>()
            .map_err(|_| TetherError::new(ErrorCategory::Configuration, "invalid bus number"))?;
        let address = parts[1]
            .parse::<u8>()
            .map_err(|_| TetherError::new(ErrorCategory::Configuration, "invalid address"))?;
        if !devices
            .iter()
            .any(|device| device.bus == bus && device.address == address)
        {
            return Err(TetherError::new(
                ErrorCategory::UsbNotFound,
                format!("no USB device at bus {bus} address {address}"),
            ));
        }
        return Ok((bus, address));
    }
    if let Ok(address) = trimmed.parse::<u8>() {
        let matches: Vec<&DeviceInfo> = devices
            .iter()
            .filter(|device| device.address == address)
            .collect();
        return match matches.as_slice() {
            [device] => Ok((device.bus, device.address)),
            [] => Err(TetherError::new(
                ErrorCategory::UsbNotFound,
                format!("no USB device with address {address}"),
            )),
            _ => Err(TetherError::new(
                ErrorCategory::Configuration,
                "address is ambiguous; use bus:address",
            )),
        };
    }
    Err(TetherError::new(
        ErrorCategory::Configuration,
        "selector must be bus:address, bus-address or an address",
    ))
}

pub fn map_rusb(error: rusb::Error) -> TetherError {
    let category = match error {
        rusb::Error::NoDevice | rusb::Error::NotFound => ErrorCategory::UsbNotFound,
        rusb::Error::Access => ErrorCategory::UsbPermission,
        rusb::Error::Busy => ErrorCategory::UsbBusy,
        rusb::Error::Timeout => ErrorCategory::UsbTimeout,
        rusb::Error::Interrupted => ErrorCategory::UsbTransfer,
        rusb::Error::BadDescriptor => ErrorCategory::MalformedDescriptor,
        _ => ErrorCategory::UsbTransfer,
    };
    TetherError::new(category, error.to_string())
}

pub fn map_rusb_result(error: rusb::Error) -> tether_core::transport::UsbError {
    use tether_core::transport::UsbError;
    match error {
        rusb::Error::Timeout => UsbError::Timeout,
        rusb::Error::Pipe => UsbError::Stall,
        rusb::Error::NoDevice | rusb::Error::NotFound => UsbError::Disconnected,
        rusb::Error::Access => UsbError::Permission,
        rusb::Error::Busy => UsbError::Busy,
        other => UsbError::Other(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tether_core::descriptor::{
        ControlInterface, DataInterface, Endpoint, Protocol, TetherFunction, TransferType,
    };

    fn bulk(address: u8) -> Endpoint {
        Endpoint {
            address,
            transfer_type: TransferType::Bulk,
            max_packet_size: 512,
            interval: 0,
            extra: Vec::new(),
        }
    }

    fn function(control: u8, data: u8) -> TetherFunction {
        TetherFunction {
            protocol: Protocol::Rndis,
            configuration_value: 1,
            control: ControlInterface {
                number: control,
                alt: 0,
                class: 0xe0,
                subclass: 0x01,
                protocol: 0x03,
                interrupt_in: None,
            },
            data: DataInterface {
                number: data,
                alt: 0,
                bulk_in: bulk(0x81),
                bulk_out: bulk(0x01),
            },
            union: None,
            iad: None,
            ethernet: None,
            ncm: None,
            functionals: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn claims_only_control_and_data_interfaces() {
        // A composite device may also expose ADB (interface 0) and MTP
        // (interface 3).  Those interfaces must never appear in the claim plan.
        assert_eq!(claim_plan(&function(1, 2)), vec![1, 2]);
        assert_eq!(claim_plan(&function(1, 1)), vec![1]);
    }
}
