# G1-Verifikation (synthetisch, ohne Android-Gerät)

Stand: 1. November 2026. Diese Datei dokumentiert den tatsächlich ausgeführten
Stand. Sie ist **kein** G2-Nachweis und enthält keine Aussage über ein reales
Samsung-Gerät.

## Umgebung

| Feld | Wert |
|---|---|
| Mac | Apple Silicon (arm64) |
| macOS | 27.2 (Build <build>) |
| Rust | rustc 1.99.1 (2026-10-13), cargo 1.99.1 |
| Ziel | `aarch64-apple-darwin` |
| libusb | vendored via `libusb1-sys` |
| Netzwerk | kein Android-Gerät angeschlossen |

## Ausgeführte Nachweise

### Formatierung und statische Prüfung

```text
cargo fmt --all -- --check                                  # erfolgreich
cargo clippy --workspace --all-targets -- -D warnings       # erfolgreich
```

### Tests

```text
cargo test --workspace --all-targets
```

Ergebnis des letzten vollständigen Laufs (`scripts/verify-g1.sh`, log in
`target/g1/tests.log`): **100 bestanden, 0 fehlgeschlagen**. Aufteilung:

| Testbinary | Tests |
|---|---:|
| `tether-cli` Unit-Tests | 2 |
| `tether-cli` Integrationstests | 4 |
| `tether-core` Unit-Tests | 28 |
| `tests/descriptors.rs` | 12 |
| `tests/negative.rs` | 9 |
| `tests/properties.rs` | 5 |
| `tests/session.rs` | 18 |
| `tether-helper` Unit-Tests | 3 (LaunchDaemon-plist) |
| `tether-macos` Unit-Tests | 18 |
| `tether-usb` Unit-Tests | 1 (Claim-Plan) |

Testanzahl und Fehlversuche werden von `scripts/verify-g1.sh` zusätzlich in
`target/g1-report.json` maschinenlesbar gespeichert. Die Testgruppen sind in
[TRACEABILITY.md](TRACEABILITY.md) aufgeführt.

Wesentliche synthetische Nachweise:

- vollständiger RNDIS-Handshake (INITIALIZE, OID-Queries, SET Packet Filter);
- bidirektionale Ethernet-Frames über Mock-USB und Mock-Brücke;
- ARP und DHCP Discover/Offer mit echten IPv4-/UDP-/BOOTP-Feldern;
- mehrere Frames pro Transfer, Reassembly über beliebige Transfergrenzen,
  Padding und USB-Short-Packet-Fall;
- Link-Down/Up-Indications, Keepalive, Reset und erneute Initialisierung;
- begrenzte Fehler-Recovery nach transientem Bulk-Fehler;
- negative Tests für falsche Request-IDs, Truncation, ungültige Offsets und
  übergroße Frames;
- Property-Tests und Fuzz-Targets ohne Panics.

### arm64-Build

```text
cargo build --release --target aarch64-apple-darwin --workspace
file target/aarch64-apple-darwin/release/android-tether
# Mach-O 64-bit executable arm64
file target/aarch64-apple-darwin/release/android-tether-helper
# Mach-O 64-bit executable arm64
```

### Fuzzing

```text
cargo +nightly fuzz build
cargo +nightly fuzz run descriptors -- -max_total_time=8
cargo +nightly fuzz run rndis       -- -max_total_time=8
cargo +nightly fuzz run ethernet    -- -max_total_time=8
```

Die Läufe endeten ohne Crash. Exakte Zähler aus dem letzten Lauf:

| Target | Ausführungen | Dauer | Coverage (libFuzzer) |
|---|---:|---:|---|
| `descriptors` | 1.878.311 | 9 s | cov 713, ft 2091 |
| `rndis` | 2.291.430 | 9 s | cov 250, ft 658 |
| `ethernet` | 3.112.690 | 9 s | cov 116, ft 154 |

Exakte Logs: `target/g1/fuzz-*.log`. Fuzzing ist zeitlich begrenzt und kein
Beweis vollständiger Abdeckung.

### CLI-Simulation

```text
android-tether simulate --scenario basic
# frames host->phone=1 phone->host=1, device received=1, host received=1

android-tether simulate --scenario dhcp --json
# protocol=RNDIS, frames je Richtung=1, delivered_to_host=1, warnings=[]

android-tether simulate --scenario faults
# reduzierter Packet-Filter 0x0d nach Ablehnung von 0x2d; Frames weiterhin
# bidirektional; Warnungen dokumentiert
```

### macOS-Integration

Der Root-Helper wurde in dieser Umgebung **nicht** gestartet (kein
passwortloses sudo). Daher:

```text
android-tether net-smoke --json
# Exit-Code 3 = übersprungen (Helper nicht als Root gestartet), nicht Erfolg:
{
  "passed": false,
  "skipped": true,
  "checks": [
    {"name":"architecture","status":"pass",...},
    {"name":"os","status":"pass",...},
    {"name":"helper_smoke","status":"skip","detail":"skipped: cannot connect ..."}
  ]
}
```

Damit sind `feth`-Paar-Erzeugung, BPF-Öffnen, `scutil --prefs`-Dienst und der
Link-up/down/Cleanup-Pfad **implementiert und unit-getestet**, aber nicht als
Root auf diesem Mac ausgeführt. `scripts/macos-smoke.sh` führt genau diesen
Test aus, sobald der Helper mit Root-Rechten gestartet werden kann.

Zusätzlich geprüft (unprivilegiert, ohne Änderungen):

```text
android-tether diagnose
# architecture=aarch64, macOS 27.2, ifconfig/scutil vorhanden,
# BPF0 permission denied (erwartet ohne Root), Helper nicht erreichbar

android-tether list --all
# USB-Geräte werden gelesen; keine Netzwerkfunktion an den angeschlossenen
# Nicht-Android-Geräten erkannt
```

## Bewertung gegen PLAN.md §7 (G1)

| G1-Kriterium | Status |
|---|---|
| 1. `cargo test` + verpflichtende CI-Prüfungen | lokal erfüllt; CI-Workflow vorhanden, aber ohne Remote nicht ausgeführt |
| 2. simulierte RNDIS-Initialisierung bis bidirektionaler Frame-Übertragung; Fehlerbehandlung | erfüllt |
| 3. alle einschlägigen Testgruppen inkl. Fuzzing | erfüllt für T-01 bis T-06/T-08-Klassifikation; T-07 nur teilweise (kein Langzeit-/Sleep-Wake) |
| 4. arm64-Build + hardwarefreier macOS-Smoke | Build erfüllt; macOS-Smoke **skipped** (Root-Helper nicht gestartet) |
| 5. Testmatrix, Quellen, Commit-ID, Logs, Lücken | Matrix/Quellen/Logs vorhanden; Commit-ID und Requirement-Abdeckung maschinenlesbar in `target/g1-report.json`; `docs/requirements.json` enthält den geprüften Status je F-/N-Anforderung |

**Ergebnis:** Kein vollständiges G1 nach der strengen Definition, weil
Kriterium 4 nur teilweise ausgeführt wurde. Der korrekte Status lautet:

> **Protokollkern synthetisch verifiziert; vollständige synthetische
> macOS-Integration noch nicht ausgeführt.** Der Rust-Protokollkern und die
> USB-/Netzwerk-Mocks bestehen die dokumentierten Konformitäts-, Integrations-
> und Fehlertests. Der arm64-macOS-Build ist erfolgreich. Eine funktionierende
> Verbindung zu einem echten Android-Gerät ist ausdrücklich **nicht**
> nachgewiesen.

## Offene Lücken

1. `net-smoke`/`connect` mit Root-Helper auf echter macOS-Instanz.
2. Hotplug, Sleep/Wake, Langzeitbetrieb.
3. NCM/ECM-Delegation auf realer Hardware.
4. Coverage-Bericht (derzeit nicht gemessen).
5. Signierung, Notarisierung, launchd-Installation.
6. G2: Samsung-Gerät, DHCP/DNS/HTTPS ohne anderen Uplink, 15-Minuten-Test,
   Wiederholungen, Cleanup.

## G2

G2 wurde nicht begonnen. Ein Testprotokoll-Template liegt in
[G2-TEMPLATE.md](G2-TEMPLATE.md).
