#![no_main]
use libfuzzer_sys::fuzz_target;
use tether_core::cdc::parse_interface_extra;
use tether_core::descriptor::{
    classify_configuration, parse_configuration, parse_device_descriptor,
};

fuzz_target!(|data: &[u8]| {
    if let Ok(config) = parse_configuration(data) {
        let _ = classify_configuration(&config);
    }
    let _ = parse_device_descriptor(data);
    let _ = parse_interface_extra(data);
});
