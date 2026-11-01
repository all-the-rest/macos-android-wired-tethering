# Traceability: Anforderungen ↔ Quellen ↔ Implementierung ↔ Tests

Stand: 1. November 2026. Diese Tabelle bezieht sich auf den Prototyp-Stand
ohne reale Geräteverifikation. Testnamen sind relative Pfade innerhalb des
Workspace; `cargo test --workspace --all-targets` führt sie aus.

## Funktionale Anforderungen

| ID | Umsetzung | Tests / Nachweis | Quellen |
|---|---|---|---|
| F-01 Geräteerkennung | `tether-core/src/descriptor.rs` (Raw-Deskriptor-Parser, normalisierte Config, IAD, CDC Union, Alternate Settings), `tether-core/src/cdc.rs`, `tether-usb/src/discover.rs` (libusb-Adapter) | `tests/descriptors.rs`: `recognizes_standard_rndis_by_class_tuple`, `recognizes_composite_rndis_iad_without_vendor_ids`, `interface_numbers_are_not_hard_coded`, `rejects_truncated_configuration`, `rejects_endpoint_count_mismatch`, `rejects_zero_length_descriptor`, `truncation_and_bit_flips_never_panic`; `tests/properties.rs` | S-02, S-03, S-05; USB 2.0 §9.6 |
| F-02 Koexistenz | `discover.rs::claim_plan` claims exactly the RNDIS control and data interfaces; `open_rndis` checks `kernel_driver_active` for both and never detaches; `set_alternate_setting` only on the data interface | `discover::tests::claims_only_control_and_data_interfaces`; Negativpfad ist implementiert, aber ohne reale Hardware nicht ausgeführt; `list`/`diagnose` verändern keine Interfaces. **Lücke:** kein automatisierter libusb-Claim gegen ein reales Composite-Gerät | S-02; PLAN F-02 |
| F-03 RNDIS (MVP) | `tether-core/src/rndis/message.rs`, `oids.rs`, `packet.rs`, `rndis/session.rs`, `rndis/mock.rs` | `message.rs` Unit-Tests (Layout, Offset-Basis +8, 44-Byte-Packet, Reset/Indicate); `tests/session.rs` (Init, OID-Queries, Filter, Link, Keepalive, Reset/Reinit, Multi-Frame); `tests/negative.rs`; Fuzz-Target `rndis` | S-01 §§2.2.1–2.2.8, 2.2.14, 3.1, 3.1.2; S-02 |
| F-04 CDC-NCM | `descriptor.rs::classify_configuration` erkennt `02/0D/00`, NCM-Funktionaldeskriptor, Dateninterface und Bulk-Paare; `DetectedFunction` meldet `DelegateNative` | `tests/descriptors.rs::identifies_ncm_and_delegates_to_native_driver` | S-03, S-04, S-05 |
| F-05 CDC-ECM | Erkennung `02/06/00` + Ethernet-Funktionaldeskriptor; Delegation | `tests/descriptors.rs::identifies_ecm_and_delegates_to_native_driver` | S-03, S-05 |
| F-06 Netzwerkintegration | `tether-core/src/net.rs` (FrameIo), `engine.rs`; `tether-macos/src/feth.rs`, `bpf.rs`, `service.rs`; macOS-DHCP/DNS/Routen durch Systemdienst; `state.rs` liest die vergebene IPv4-Adresse für `status` | `tests/session.rs` Basic/DHCP/Faults; `packet.rs` Reassembly; `feth.rs`/`service.rs`/`bpf.rs` Unit-Tests; `android-tether net-smoke` (benötigt Root-Helper) | S-07 soweit relevant; PLAN F-06 |
| F-07 Lebenszyklus | `engine.rs` (Link-Status, Reset, begrenzte Reconnect-Versuche, Halt, Link down, `PowerHooks`), `tether-macos/src/power.rs` (IOKit `WillSleep`/`HasPoweredOn`), `tether-cli` Reconnect-Supervisor (feth/BPF bleibt erhalten, Host-MAC wird gepinnt, Timeout per `--reconnect-timeout`), `testkit.rs` Reconnect-Szenario, `tether-helper` räumt bei Socket-Ende auf, `cli disconnect/cleanup` | `tests/session.rs::engine_recovers_from_a_transient_transfer_error`, `engine_run_stops_and_marks_link_down`, `media_indications_update_link_state`, `repeated_reset_and_initialize_cycles_stay_correlated`, `pinned_host_mac_overrides_the_device_value`, `reconnect_simulation_reuses_bridge_and_pins_host_mac`, `sleep_and_wake_events_update_link_and_reinitialize`; `power::tests`; **Lücken:** realer Sleep/Wake, Replug und Gerätewechsel nicht auf Hardware getestet | S-01 §3.1; PLAN F-07 |
| F-08 Diagnostik | `tether-cli/src/main.rs`: `list`, `status`, `diagnose`, `connect`, `disconnect`, `cleanup`, `simulate`, `net-smoke`; `state.rs` ohne Seriennummern | CLI-Läufe in `docs/G1-VERIFICATION.md`; `state.rs` Unit-Test; `simulate --json` | PLAN F-08 |
| F-09 Installation/Deinstallation | arm64-Build dokumentiert; Helper mit festen Operationen; fester Service-UUID; `cleanup` entfernt nur eigene UUID; `android-tether-helper install/uninstall` verwaltet ein LaunchDaemon mit festem Label und entfernt den reservierten Dienst | `feth.rs`/`service.rs` Cleanup-Tests, `install.rs` plist-Tests, `net-smoke` Cleanup-Check (skipped ohne Root); **Lücke:** Signierung/Notarisierung, kein realer Installationslauf | PLAN F-09, N-06 |

## Nichtfunktionale Anforderungen

| ID | Umsetzung | Nachweis | Quellen |
|---|---|---|---|
| N-01 Sicherheit | Bounds-checks in allen Parsern; `checked_add`; max. Puffer; keine `unsafe` im Protokollkern; FFI nur in `tether-macos`/libusb mit Dokumentation | `tests/negative.rs`, `tests/properties.rs`, Fuzz-Targets; `cargo clippy -D warnings` | S-01; PLAN N-01 |
| N-02 Robustheit | Timeouts, begrenzte Poll-Versuche, Reassembly-Limit, Reconnect-Backoff, Halt/Link-down im Fehlerpfad | `tests/negative.rs`; `engine_recovers...`; Fuzz | PLAN N-02 |
| N-03 Testbarkeit | `UsbTransport`-Trait + `MockRndisDevice`, `FrameIo` + `MockFrameIo`, `testkit.rs` | Alle Tests ohne USB/Root; `simulate` | PLAN N-03 |
| N-04 Wartbarkeit | Rust-Workspace, fmt/clippy/test, CI-Workflow, Fuzz-Targets, arm64-Build | `scripts/verify-g1.sh`, `.github/workflows/ci.yml`, `docs/G1-VERIFICATION.md`; **Lücke:** Coverage-Bericht nicht gemessen | PLAN N-04 |
| N-05 Datenschutz | keine Telemetrie; State-/Logfelder ohne Nutzdaten/Seriennummern; Diagnose nennt Bus/Adresse/VID/PID | `state.rs` Test `roundtrip_state_json`; CLI-Ausgaben | PLAN N-05 |
| N-06 Distribution | nicht umgesetzt (bewusst außerhalb des Prototyps) | – | PLAN N-06 |

## Testgruppen

| ID | Tests | Ergebnis / Abdeckung |
|---|---|---|
| T-01 Deskriptoren | `tests/descriptors.rs` (12 Tests), `tests/properties.rs` (arbitrary bytes) | bestanden; Deckung: RNDIS, Composite-RNDIS, NCM, ECM, Alternate Settings, variable Interface-Nummern, Truncation, Bitflips, fehlende Data-Interfaces. **Lücke:** keine realen Hersteller-Deskriptoren |
| T-02 RNDIS-Nachrichten | `rndis/message.rs` Unit-Tests; `tests/negative.rs` | bestanden; Golden Vectors aus Feldlayouts konstruiert, Offset-Basis +8 explizit getestet |
| T-03 Ethernet-Daten | `ethernet.rs`, `rndis/packet.rs` Unit-Tests; `tests/session.rs` | bestanden; ARP, DHCP Discover/Offer, Multi-Frame, Split-Reassembly, Padding, kurze Transfers |
| T-04 Negativ/Sicherheit | `tests/negative.rs`, `tests/properties.rs`, Fuzz-Targets | bestanden; Fuzz-Läufe in `docs/G1-VERIFICATION.md` |
| T-05 Zustandsmaschine | `tests/session.rs` | bestanden; Init/Query/Set/Filter/Keepalive/Reset/Reinit/Link-Indications/Recovery |
| T-06 Host-Brücke | `testkit.rs`, `simulate`; `bpf.rs`/`feth.rs`/`service.rs`; `net-smoke` | Mock-Integration bestanden; macOS-Runtime-Smoke **skipped** (Root-Helper nicht gestartet) |
| T-07 Betrieb | `engine_run_stops...`, wiederholte Reset-Zyklen, Sleep/Wake-Hooks, Reconnect-Szenario, Fuzz | synthetisch bestanden; keine Langzeit-/Hardware-Sleep-Wake-/Replug-Tests |
| T-08 NCM/ECM | `tests/descriptors.rs` | bestanden für Klassifikation/Delegation; kein eigener NCM/ECM-Stack (bewusst) |

## Quellenbezug

- **S-01** [MS-RNDIS], Microsoft, Remote Network Driver Interface Specification,
  2014-05-01: §2.2.1 Message Header, §2.2.2 Initialize, §2.2.3 Halt,
  §2.2.4 Query, §2.2.5 Set, §2.2.6 Reset, §2.2.7 Indicate Status,
  §2.2.8 Keepalive, §2.2.14 Packet Message, §3.1 State Machine, §3.1.2 Timer.
- **S-02** Microsoft, Remote NDIS to USB Mapping: Control/Data Channels,
  Encapsulated Commands, Endpoint-Anforderungen.
- **S-03** USB-IF CDC Class Definitions: Communication Class, Data Class,
  Union Functional Descriptor, Ethernet Networking Functional Descriptor.
- **S-04** USB-IF Network Control Model (NCM) 1.1: Klassencode,
  NCM-Funktionaldeskriptor, NTB (nur Klassifikation/Delegation).
- **S-05** USB-IF Defined Class Codes: Basisklassen/Subklassen/Protokolle.
- **S-06** AOSP Tethering-Konfiguration: nur Hintergrund, keine
  Hardware-Kompatibilitätsaussage.
- **S-07/S-08/S-09** Apple NetworkingDriverKit, USBDriverKit,
  DriverKit-Entitlements: Alternativenbewertung, nicht implementiert.

## Offene Lücken (bewusst, kein G2-Nachweis)

1. Kein Lauf gegen ein echtes Android-/Samsung-Gerät; keine DHCP-/DNS-/HTTPS-
   Verifikation über Mobilfunk.
2. `net-smoke` und `connect` wurden ohne Root-Helper nicht real ausgeführt.
3. Kein Hardware-Sleep/Wake-/Replug-/Langzeit-Test; die IOKit-Power-Hooks und
   der Reconnect-Supervisor sind synthetisch getestet.
4. NCM/ECM werden nicht selbst gefahren; Delegation an macOS ist nur
   klassifiziert, nicht auf Hardware geprüft.
5. Keine Coverage-Messung; Fuzzing lief zeitlich begrenzt (ca. 8–9 s je Target).
6. Keine Signierung/Notarisierung/launchd-Installation.
