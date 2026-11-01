#![no_main]
use libfuzzer_sys::fuzz_target;
use tether_core::rndis::message::{
    parse_header, parse_indicate_status, parse_initialize_complete, parse_packet,
    parse_query_complete, parse_reset_complete, parse_set_complete, split_packet_transfer,
};
use tether_core::rndis::packet::PacketReassembler;

fuzz_target!(|data: &[u8]| {
    let _ = parse_header(data);
    if let Ok(header) = parse_header(data) {
        match header.kind {
            tether_core::rndis::message::MSG_INITIALIZE_CMPLT => {
                let _ = parse_initialize_complete(data);
            }
            tether_core::rndis::message::MSG_QUERY_CMPLT => {
                let _ = parse_query_complete(data);
            }
            tether_core::rndis::message::MSG_SET_CMPLT => {
                let _ = parse_set_complete(data);
            }
            tether_core::rndis::message::MSG_RESET_CMPLT => {
                let _ = parse_reset_complete(data);
            }
            tether_core::rndis::message::MSG_INDICATE_STATUS => {
                let _ = parse_indicate_status(data);
            }
            tether_core::rndis::message::MSG_PACKET => {
                let _ = parse_packet(data);
            }
            _ => {}
        }
    }
    let _ = split_packet_transfer(data);
    let mut reassembler = PacketReassembler::new(128 * 1024);
    let split = data.len() / 3;
    let _ = reassembler.push(&data[..split]);
    let _ = reassembler.push(&data[split..]);
});
