# RNDIS-Protokollnotizen und Testvektoren

Grundlage: Microsoft [MS-RNDIS] (2014-05-01), Microsoft „Remote NDIS to USB
Mapping“ und USB-IF CDC/NCM-Definitionen. Alle mehrbyte-Felder sind
little-endian. Der Codec liegt in `crates/tether-core/src/rndis/`.

## Nachrichten-Header

Jede Control-Nachricht beginnt mit:

| Offset | Länge | Feld |
|---|---|---|
| 0 | 4 | `MessageType` |
| 4 | 4 | `MessageLength` (inkl. Header und Padding) |
| 8 | 4 | `RequestId` (nicht bei Reset/Indicate) |

Nachrichten-Typen: `0x00000002` INITIALIZE, `0x80000002` INITIALIZE_CMPLT,
`0x00000004` QUERY, `0x80000004` QUERY_CMPLT, `0x00000005` SET,
`0x80000005` SET_CMPLT, `0x00000006` RESET, `0x80000006` RESET_CMPLT,
`0x00000007` INDICATE_STATUS, `0x00000008` KEEPALIVE,
`0x80000008` KEEPALIVE_CMPLT, `0x00000001` PACKET.

## Offset-Basis

Die Offset-Felder in QUERY, SET, QUERY_CMPLT und PACKET_MSG sind relativ zum
**Beginn des Offset-Feldes**, also relativ zu `MessageStart + 8`:

- QUERY/SET: Header 28 Byte, `InformationBufferOffset = 20` für direkt
  folgende Daten (absolute Position 28).
- QUERY_CMPLT: Header 24 Byte, Offset-Feldwert `16` für direkt folgende Daten
  (absolute Position 24).
- PACKET_MSG: Header 44 Byte, `DataOffset = 36` für direkt folgende Daten
  (absolute Position 44).

`REMOTE_NDIS_INDICATE_STATUS_MSG` ist die dokumentierte Ausnahme: sein
`StatusBufferOffset` ist relativ zum Nachrichtenbeginn. Der Parser akzeptiert
einen ungültigen optionalen Statuspuffer, ohne den Status selbst zu verwerfen.

Diese Basis ist in `message.rs` explizit getestet
(`query_complete_offset_is_relative_to_field_base`,
`indicate_status_buffer_offset_is_from_message_start`,
`packet_roundtrip_and_alignment`).

## INITIALIZE

Host-Request: 24 Byte (`MessageType`, `MessageLength`, `RequestId`,
`MajorVersion=1`, `MinorVersion=0`, `MaxTransferSize`).

Completion: 52 Byte, u. a.:

| Offset | Feld |
|---|---|
| 12 | Status |
| 16 | MajorVersion |
| 20 | MinorVersion |
| 24 | DeviceFlags (Bit 0 = connectionless) |
| 28 | Medium (0 = 802.3) |
| 32 | MaxPacketsPerTransfer |
| 36 | MaxTransferSize |
| 40 | PacketAlignmentFactor (Exponent; Faktor = 2^n) |
| 44 | AFListOffset |
| 48 | AFListSize |

Der Host verlangt 802.3, `MaxPacketsPerTransfer >= 1`, `MaxTransferSize >= 44`
und einen Alignment-Exponenten `<= 7`.

## QUERY / SET / OIDs

Relevante OIDs: `OID_802_3_PERMANENT_ADDRESS` (designierte Host-MAC),
`OID_802_3_CURRENT_ADDRESS`, `OID_GEN_MAXIMUM_FRAME_SIZE`,
`OID_GEN_MAXIMUM_TOTAL_SIZE`, `OID_GEN_LINK_SPEED`,
`OID_GEN_MEDIA_CONNECT_STATUS`, `OID_GEN_PHYSICAL_MEDIUM`,
`OID_GEN_CURRENT_PACKET_FILTER`, `OID_802_3_MULTICAST_LIST`.

Packet-Filter: `DIRECTED | BROADCAST | ALL_MULTICAST | PROMISCUOUS`
(`0x2D`, Linux-Default). Der Host versucht bei Ablehnung schrittweise
`DIRECTED|BROADCAST|ALL_MULTICAST` (`0x0D`) und
`DIRECTED|BROADCAST` (`0x09`). Ohne erfolgreichen Filter wird die Session
abgelehnt.

## RESET, HALT, KEEPALIVE

- RESET-Request ist 12 Byte; RESET_CMPLT ist 16 Byte mit `Status` bei Offset 8
  und `AddressingReset` bei 12 (kein RequestId-Feld).
- HALT wird gesendet, ohne auf eine Completion zu warten.
- Keepalive wird nach 5 s Control-Idle gesendet; der Host beantwortet auch
  Keepalive-Nachrichten des Geräts.

## PACKET_MSG

Header 44 Byte:

| Offset | Feld |
|---|---|
| 0 | MessageType = 1 |
| 4 | MessageLength (inkl. Padding) |
| 8 | DataOffset (Basis +8) |
| 12 | DataLength |
| 16 | OOBDataOffset |
| 20 | OOBDataLength |
| 24 | NumOOBDataElements |
| 28 | PerPacketInfoOffset |
| 32 | PerPacketInfoLength |
| 36 | VcHandle |
| 40 | Reserved |

Ein USB-Bulk-Transfer kann mehrere PACKET_MSG enthalten. Der Parser trennt
alle Nachrichten anhand `MessageLength`, prüft Data-/OOB-/Per-Packet-Offsets
und puffert Nachrichten, die über Transfergrenzen reichen. Die maximale
Reassembly-Größe ist begrenzt.

Beim Senden wird ein Ethernet-Frame in eine PACKET_MSG gekapselt
(`DataOffset=36`). Die Nachricht wird auf den vom Gerät gemeldeten
Alignment-Faktor aufgefüllt. Endet ein Transfer exakt auf einem Vielfachen der
Bulk-Max-Packet-Größe, wird ein einzelnes Null-Byte als USB-Short-Packet-Pad
angehängt (kein echter ZLP); der Empfänger toleriert dieses Byte.

## Linkstatus

- `OID_GEN_MEDIA_CONNECT_STATUS`: 0 = connected, 1 = disconnected.
- `REMOTE_NDIS_INDICATE_STATUS_MSG`: Status `0x4001000B` = MEDIA_CONNECT,
  `0x4001000C` = MEDIA_DISCONNECT. Optional kann ein Statuspuffer den
  Media-OID und Wert enthalten.
- Die Engine setzt das `feth`-Host-Interface bei Disconnect down und bei
  Connect wieder up.

## Klassifikation (CDC)

- RNDIS-Control: `E0/01/03`, `02/02/FF`, `EF/04/01`, `EF/01/01`; Data: `0A`.
- NCM: Control `02/0D/00`, NCM-Funktionaldeskriptor `0x1A`, Data `0A`.
- ECM: Control `02/06/00`, Ethernet-Funktionaldeskriptor `0x0F`, Data `0A`.
- Data-Interface wird bevorzugt über CDC Union, dann über IAD, dann über ein
  CDC-Data-Interface ermittelt. Das Bulk-Paar wird im niedrigsten Alternate
  Setting mit IN und OUT gesucht.

NCM/ECM werden nur klassifiziert und an den nativen macOS-Treiber delegiert;
der Prototyp detacht keine nativen Treiber.

## Nicht abgedeckt

- volle NCM-NTB-/NDP-Verarbeitung,
- eigene ECM-Datenpfade,
- IPv6-spezifische Verifikation,
- herstellerspezifische RNDIS-Varianten (z. B. MAC-Fixups) über die
  dokumentierten Klassen hinaus.
