use tether_core::descriptor::{
    classify_configuration, parse_configuration, Protocol, SupportLevel,
};

// The fixtures below are built field-by-field from USB 2.0 descriptor layouts
// and the CDC/RNDIS class definitions.  They are deliberately independent of
// the production encoder (there is no production descriptor encoder).

struct Fixture {
    bytes: Vec<u8>,
    interfaces: u8,
}

impl Fixture {
    fn new(interfaces: u8) -> Self {
        let mut bytes = vec![0u8; 9];
        bytes[0] = 9;
        bytes[1] = 0x02;
        bytes[2] = 0;
        bytes[3] = 0;
        bytes[4] = interfaces;
        bytes[5] = 1; // configuration value
        bytes[6] = 0;
        bytes[7] = 0x80;
        bytes[8] = 50; // 100 mA
        Self { bytes, interfaces }
    }

    fn raw(mut self, descriptor: &[u8]) -> Self {
        self.bytes.extend_from_slice(descriptor);
        self
    }

    fn interface(
        self,
        number: u8,
        alt: u8,
        endpoints: u8,
        class: u8,
        subclass: u8,
        protocol: u8,
    ) -> Self {
        self.raw(&[
            9, 0x04, number, alt, endpoints, class, subclass, protocol, 0,
        ])
    }

    fn endpoint(self, address: u8, attributes: u8, max_packet: u16, interval: u8) -> Self {
        let [lo, hi] = max_packet.to_le_bytes();
        self.raw(&[7, 0x05, address, attributes, lo, hi, interval])
    }

    fn cdc_header(self) -> Self {
        self.raw(&[5, 0x24, 0x00, 0x10, 0x01])
    }

    fn cdc_union(self, control: u8, data: u8) -> Self {
        self.raw(&[5, 0x24, 0x06, control, data])
    }

    fn cdc_ethernet(self, max_segment: u16) -> Self {
        let [lo, hi] = max_segment.to_le_bytes();
        self.raw(&[13, 0x24, 0x0f, 0, 0, 0, 0, 0, lo, hi, 0, 0, 0])
    }

    fn cdc_ncm(self) -> Self {
        self.raw(&[6, 0x24, 0x1a, 0x00, 0x01, 0x00])
    }

    fn iad(self, first: u8, count: u8, class: u8, subclass: u8, protocol: u8) -> Self {
        self.raw(&[8, 0x0b, first, count, class, subclass, protocol, 0])
    }

    fn finish(mut self) -> Vec<u8> {
        let total = self.bytes.len() as u16;
        self.bytes[2..4].copy_from_slice(&total.to_le_bytes());
        self.bytes[4] = self.interfaces;
        self.bytes
    }
}

fn rndis_fixture() -> Vec<u8> {
    Fixture::new(2)
        .interface(0, 0, 1, 0xe0, 0x01, 0x03)
        .cdc_header()
        .raw(&[5, 0x24, 0x01, 0x00, 0x01]) // call management
        .raw(&[4, 0x24, 0x02, 0x02]) // ACM
        .cdc_union(0, 1)
        .endpoint(0x82, 0x03, 64, 9)
        .interface(1, 0, 0, 0x0a, 0x00, 0x00)
        .interface(1, 1, 2, 0x0a, 0x00, 0x00)
        .endpoint(0x81, 0x02, 512, 0)
        .endpoint(0x01, 0x02, 512, 0)
        .finish()
}

fn ncm_fixture() -> Vec<u8> {
    Fixture::new(2)
        .interface(0, 0, 1, 0x02, 0x0d, 0x00)
        .cdc_header()
        .cdc_union(0, 1)
        .cdc_ncm()
        .cdc_ethernet(1500)
        .endpoint(0x82, 0x03, 16, 4)
        .interface(1, 0, 0, 0x0a, 0x00, 0x00)
        .interface(1, 1, 2, 0x0a, 0x00, 0x00)
        .endpoint(0x81, 0x02, 512, 0)
        .endpoint(0x01, 0x02, 512, 0)
        .finish()
}

fn ecm_fixture() -> Vec<u8> {
    Fixture::new(2)
        .interface(0, 0, 1, 0x02, 0x06, 0x00)
        .cdc_header()
        .cdc_union(0, 1)
        .cdc_ethernet(1500)
        .endpoint(0x82, 0x03, 16, 4)
        .interface(1, 0, 2, 0x0a, 0x00, 0x00)
        .endpoint(0x81, 0x02, 512, 0)
        .endpoint(0x01, 0x02, 512, 0)
        .finish()
}

#[test]
fn recognizes_standard_rndis_by_class_tuple() {
    let config = parse_configuration(&rndis_fixture()).expect("valid fixture");
    let detected = classify_configuration(&config);
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].protocol, Protocol::Rndis);
    assert_eq!(detected[0].support, SupportLevel::Usable);
    let function = detected[0].usable().expect("usable RNDIS");
    assert_eq!(function.control.number, 0);
    assert_eq!(function.data.number, 1);
    assert_eq!(function.data.alt, 1);
    assert_eq!(function.data.bulk_in.address, 0x81);
    assert_eq!(function.data.bulk_out.address, 0x01);
    assert_eq!(
        function.control.interrupt_in.as_ref().unwrap().address,
        0x82
    );
}

#[test]
fn recognizes_composite_rndis_iad_without_vendor_ids() {
    let bytes = Fixture::new(2)
        .iad(0, 2, 0xef, 0x04, 0x01)
        .interface(0, 0, 1, 0x02, 0x02, 0xff)
        .cdc_header()
        .cdc_union(0, 1)
        .endpoint(0x82, 0x03, 8, 9)
        .interface(1, 0, 2, 0x0a, 0x00, 0x00)
        .endpoint(0x81, 0x02, 64, 0)
        .endpoint(0x01, 0x02, 64, 0)
        .finish();
    let config = parse_configuration(&bytes).unwrap();
    let detected = classify_configuration(&config);
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].protocol, Protocol::Rndis);
    assert_eq!(detected[0].support, SupportLevel::Usable);
}

#[test]
fn identifies_ncm_and_delegates_to_native_driver() {
    let config = parse_configuration(&ncm_fixture()).unwrap();
    let detected = classify_configuration(&config);
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].protocol, Protocol::Ncm);
    assert_eq!(detected[0].support, SupportLevel::DelegateNative);
    let function = detected[0].function.as_ref().unwrap();
    assert!(function.ncm.is_some());
    assert_eq!(function.ethernet.as_ref().unwrap().max_segment_size, 1500);
}

#[test]
fn identifies_ecm_and_delegates_to_native_driver() {
    let config = parse_configuration(&ecm_fixture()).unwrap();
    let detected = classify_configuration(&config);
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].protocol, Protocol::Ecm);
    assert_eq!(detected[0].support, SupportLevel::DelegateNative);
}

#[test]
fn unsupported_when_data_interface_missing() {
    let bytes = Fixture::new(1)
        .interface(0, 0, 1, 0xe0, 0x01, 0x03)
        .cdc_header()
        .endpoint(0x82, 0x03, 8, 9)
        .finish();
    let config = parse_configuration(&bytes).unwrap();
    let detected = classify_configuration(&config);
    assert_eq!(detected.len(), 1);
    assert_eq!(detected[0].support, SupportLevel::Unsupported);
    assert!(detected[0].reason.contains("CDC-Data"));
}

#[test]
fn rejects_truncated_configuration() {
    let bytes = rndis_fixture();
    for length in 0..bytes.len() {
        let result = parse_configuration(&bytes[..length]);
        if length >= 9 && length < bytes.len() {
            // Most prefixes are shorter than wTotalLength and must fail; a
            // prefix that happens to contain a complete, smaller descriptor
            // chain is not possible because wTotalLength stays fixed.
            assert!(
                result.is_err(),
                "prefix of length {length} unexpectedly parsed"
            );
        } else if length < 9 {
            assert!(
                result.is_err(),
                "prefix of length {length} unexpectedly parsed"
            );
        }
    }
    assert!(parse_configuration(&bytes).is_ok());
}

#[test]
fn rejects_endpoint_count_mismatch() {
    let mut bytes = rndis_fixture();
    // Interface 0 declares one endpoint; change it to two.
    let interface_zero = bytes
        .windows(2)
        .position(|window| window == [9, 0x04])
        .expect("interface descriptor");
    bytes[interface_zero + 4] = 2;
    assert!(parse_configuration(&bytes).is_err());
}

#[test]
fn rejects_zero_length_descriptor() {
    let mut bytes = rndis_fixture();
    let marker = bytes
        .windows(2)
        .position(|window| window == [5, 0x24])
        .expect("CDC header");
    bytes[marker] = 0;
    assert!(parse_configuration(&bytes).is_err());
}

#[test]
fn rejects_duplicate_alternate_setting() {
    let bytes = Fixture::new(1)
        .interface(0, 0, 0, 0x0a, 0x00, 0x00)
        .interface(0, 0, 0, 0x0a, 0x00, 0x00)
        .finish();
    assert!(parse_configuration(&bytes).is_err());
}

#[test]
fn non_network_device_is_not_classified() {
    let bytes = Fixture::new(1)
        .interface(0, 0, 2, 0x08, 0x06, 0x50) // mass storage
        .endpoint(0x81, 0x02, 512, 0)
        .endpoint(0x02, 0x02, 512, 0)
        .finish();
    let config = parse_configuration(&bytes).unwrap();
    assert!(classify_configuration(&config).is_empty());
}

#[test]
fn truncation_and_bit_flips_never_panic() {
    let bytes = rndis_fixture();
    for length in 0..=bytes.len() {
        let _ = parse_configuration(&bytes[..length]);
    }
    for index in 0..bytes.len() {
        for bit in 0..8 {
            let mut mutated = bytes.clone();
            mutated[index] ^= 1 << bit;
            let _ = parse_configuration(&mutated);
        }
    }
}

#[test]
fn interface_numbers_are_not_hard_coded() {
    let bytes = Fixture::new(2)
        .interface(3, 0, 1, 0xe0, 0x01, 0x03)
        .cdc_header()
        .cdc_union(3, 4)
        .endpoint(0x85, 0x03, 8, 9)
        .interface(4, 0, 0, 0x0a, 0x00, 0x00)
        .interface(4, 1, 2, 0x0a, 0x00, 0x00)
        .endpoint(0x83, 0x02, 512, 0)
        .endpoint(0x04, 0x02, 512, 0)
        .finish();
    let config = parse_configuration(&bytes).unwrap();
    let detected = classify_configuration(&config);
    let function = detected[0].usable().unwrap();
    assert_eq!(function.control.number, 3);
    assert_eq!(function.data.number, 4);
    assert_eq!(function.data.bulk_in.address, 0x83);
    assert_eq!(function.data.bulk_out.address, 0x04);
}
