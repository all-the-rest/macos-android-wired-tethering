use proptest::prelude::*;
use tether_core::cdc::parse_interface_extra;
use tether_core::descriptor::{
    classify_configuration, parse_configuration, parse_device_descriptor,
};
use tether_core::ethernet::{parse_dhcp_packet, parse_ethernet};
use tether_core::rndis::message::{
    parse_header, parse_indicate_status, parse_initialize_complete, parse_packet,
    parse_query_complete, parse_reset_complete, parse_set_complete, split_packet_transfer,
};
use tether_core::rndis::packet::{encode_packet_message, PacketReassembler};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn arbitrary_configuration_bytes_never_panic(data in prop::collection::vec(any::<u8>(), 0..8192)) {
        if let Ok(config) = parse_configuration(&data) {
            let _ = classify_configuration(&config);
        }
        let _ = parse_device_descriptor(&data);
        let _ = parse_interface_extra(&data);
    }

    #[test]
    fn arbitrary_rndis_messages_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = parse_header(&data);
        if let Ok(header) = parse_header(&data) {
            match header.kind {
                tether_core::rndis::message::MSG_INITIALIZE_CMPLT => { let _ = parse_initialize_complete(&data); }
                tether_core::rndis::message::MSG_QUERY_CMPLT => { let _ = parse_query_complete(&data); }
                tether_core::rndis::message::MSG_SET_CMPLT => { let _ = parse_set_complete(&data); }
                tether_core::rndis::message::MSG_RESET_CMPLT => { let _ = parse_reset_complete(&data); }
                tether_core::rndis::message::MSG_INDICATE_STATUS => { let _ = parse_indicate_status(&data); }
                tether_core::rndis::message::MSG_PACKET => { let _ = parse_packet(&data); }
                _ => {}
            }
        }
        let _ = split_packet_transfer(&data);
    }

    #[test]
    fn packet_encoder_parser_roundtrip(len in 1usize..1518, alignment in prop::sample::select(vec![1usize,2,4,8,16])) {
        let frame: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let message = encode_packet_message(&frame, alignment).unwrap();
        let packet = parse_packet(&message).unwrap();
        prop_assert_eq!(packet.data, frame.as_slice());
        prop_assert_eq!(packet.message_length, message.len());
    }

    #[test]
    fn reassembly_of_arbitrary_chunks_never_panics(chunks in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..300), 0..40)) {
        let mut reassembler = PacketReassembler::new(64 * 1024);
        for chunk in chunks {
            let _ = reassembler.push(&chunk);
        }
    }

    #[test]
    fn arbitrary_ethernet_and_dhcp_never_panic(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = parse_ethernet(&data);
        let _ = parse_dhcp_packet(&data);
    }
}
