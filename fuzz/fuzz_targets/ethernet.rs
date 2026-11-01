#![no_main]
use libfuzzer_sys::fuzz_target;
use tether_core::ethernet::{parse_dhcp_packet, parse_ethernet};

fuzz_target!(|data: &[u8]| {
    let _ = parse_ethernet(data);
    let _ = parse_dhcp_packet(data);
});
