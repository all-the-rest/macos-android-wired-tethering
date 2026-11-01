# G1-Verifikation (synthetisch, ohne Android-Gerät)

Stand: 1. November 2026. Diese Datei dokumentiert den tatsächlich ausgeführten
G1-Stand. Sie ist **kein** G2-Nachweis; der reale Samsung-Lauf ist separat in
[G2-VERIFICATION.md](G2-VERIFICATION.md) protokolliert.

## Umgebung

| Feld | Wert |
|---|---|
| Mac | Apple Silicon (arm64) |
| macOS | 27.x (Build redacted) |
| Rust | rustc 1.99.1 (2026-10-13), cargo 1.99.1 |
| Ziel | `aarch64-apple-darwin` |
| libusb | vendored via `libusb1-sys` |
| Netzwerk | G1 nutzt kein Android-Gerät; der reale G2-Lauf ist separat dokumentiert |

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

Ergebnis des letzten vollständigen Laufs (`cargo test --workspace`, Log in
`target/g1/test-workspace.log`): **138 bestanden, 0 fehlgeschlagen**.
Aufteilung:

| Testbinary | Tests |
|---|---:|
| `tether-cli` Unit-Tests | 14 (State, PID, IPv4, ifconfig, Helper-Fehler, CREATE-Parser, LaunchAgent-Plist) |
| `tether-cli` Integrationstests | 5 |
| `tether-core` Unit-Tests | 28 |
| `tests/descriptors.rs` | 12 |
| `tests/negative.rs` | 9 |
| `tests/properties.rs` | 5 |
| `tests/session.rs` | 25 (Host-MAC, Ordnung, Größen-Clamp, Reconnect, Sleep/Wake) |
| `tether-helper` Unit-Tests | 7 (plist, Socket-Protokoll, CREATE/LINK/PREFER/FALLBACK/STATUS/DESTROY, Default-Route-Fehler) |
| `tether-macos` Unit-Tests | 30 (inkl. IOKit-Power-Monitor, Darwin-BPF-Header und Dienstreihenfolge) |
| `tether-usb` Unit-Tests | 3 (Claim-Plan, Selektor-Auflösung) |

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
- begrenzter Reconnect nach Geräteverlust: feth/BPF/Netzwerkdienst bleiben
  erhalten, die neue RNDIS-Session pinnt die bestehende Host-MAC;
- explizite IOKit-Schlaf-/Wach-Benachrichtigungen: `WillSleep` nimmt den
  Host-Link down, `HasPoweredOn` erzwingt RESET und vollständige
  Reinitialisierung der RNDIS-Session;
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

### Coverage

```text
cargo llvm-cov --workspace --json --summary-only \
  --output-path target/g1/coverage.json
```

Letzter Lauf (exakte Werte in `target/g1-report.json` und
`target/g1/coverage.json`): rund **61 % Zeilen**, **64 % Regionen** und
**59 % Funktionen**. Die
Protokollkern-Module liegen deutlich höher (z. B. `rndis/message.rs` 85,6 %,
`rndis/packet.rs` 86,0 %, `descriptor.rs` 83,2 % Zeilen). CLI-, Helper- und
BPF-Pfade, die Root oder echte Hardware benötigen, sind dagegen kaum
ausgeführt. LLVM liefert für dieses Target keine Branch-Coverage; die Zahl ist
deshalb bewusst nicht als Branch-Abdeckung ausgewiesen. Der Bericht ist
maschinenlesbar in `target/g1/coverage.json` und wird von
`scripts/verify-g1.sh` in `target/g1-report.json` zusammengefasst.

### CLI-Simulation

```text
android-tether simulate --scenario basic
# frames host->phone=1 phone->host=1, device received=1, host received=1

android-tether simulate --scenario dhcp --json
# protocol=RNDIS, frames je Richtung=1, delivered_to_host=1, warnings=[]

android-tether simulate --scenario faults
# reduzierter Packet-Filter 0x0d nach Ablehnung von 0x2d; Frames weiterhin
# bidirektional; Warnungen dokumentiert

android-tether simulate --scenario reconnect
# zwei Sessions auf derselben Mock-Bruecke; die zweite Session meldet eine
# andere Host-MAC, wird aber auf die bestehende Bridge-MAC gepinnt;
# reconnects=1, je Richtung 2 Frames
```

### macOS-Integration (privilegierter Lauf)

Der Root-Helper wurde als LaunchDaemon installiert und gestartet:

```text
android-tether-helper install --allow-uid 501
# installed all-the-rest.android-tether.helper
# binary: /Library/PrivilegedHelperTools/android-tether-helper
# plist:  /Library/LaunchDaemons/all-the-rest.android-tether.helper.plist
# socket: /var/run/android-tether-helper.sock

launchctl print system/all-the-rest.android-tether.helper
# state = running, program = /Library/PrivilegedHelperTools/android-tether-helper
```

`android-tether net-smoke --json` endet mit Exit-Code 0, `passed=true`,
`skipped=false` und elf bestandenen Checks (Auszug ohne die beiden
Umgebungschecks `architecture` und `os`):

```json
{
  "checks": [
    {"name": "feth_pair", "detail": "feth100 <-> feth1100", "status": "pass"},
    {"name": "bpf_descriptor", "detail": "passed via SCM_RIGHTS and usable", "status": "pass"},
    {"name": "host_interface", "detail": "present", "status": "pass"},
    {"name": "link_up", "detail": "ok", "status": "pass"},
    {"name": "link_state", "detail": "host interface reports UP", "status": "pass"},
    {"name": "link_down", "detail": "ok", "status": "pass"},
    {"name": "network_service", "detail": "present while session active", "status": "pass"},
    {"name": "fallback_priority", "detail": "placed behind the current default uplink", "status": "pass"},
    {"name": "cleanup", "detail": "interface removed", "status": "pass"}
  ],
  "passed": true,
  "skipped": false
}
```

Rohdaten: `target/g1/net-smoke.json`. Damit sind `feth`-Paar-Erzeugung,
BPF-Öffnen und -Lesen, `SCM_RIGHTS`-Übergabe, `scutil --prefs`-Dienst,
Link-up/down und Cleanup auf dieser macOS-Instanz als Root ausgeführt.

Zusätzlich geprüft:

```text
android-tether diagnose
# architecture=aarch64, macOS 27.2, ifconfig/scutil vorhanden,
# helper=pass, BPF0 ohne Root weiterhin permission denied (erwartet)

android-tether list --all
# Samsung 04e8:6863 als RNDIS usable erkannt; andere USB-Geräte ohne
# Netzwerkfunktion bleiben unangetastet
```

## Bewertung gegen PLAN.md §7 (G1)

| G1-Kriterium | Status |
|---|---|
| 1. `cargo test` + verpflichtende CI-Prüfungen | lokal erfüllt; erster CI-Lauf auf GitHub Actions grün: `verify`, `coverage`, `fuzz` und `macos-smoke` (11/11 Checks, nicht übersprungen), Lauf `41339266007` |
| 2. simulierte RNDIS-Initialisierung bis bidirektionaler Frame-Übertragung; Fehlerbehandlung | erfüllt |
| 3. alle einschlägigen Testgruppen inkl. Fuzzing | erfüllt für T-01 bis T-06/T-08-Klassifikation; T-07 synthetisch inkl. Sleep/Wake, aber ohne Langzeit-/Hardware-Lauf |
| 4. arm64-Build + hardwarefreier macOS-Smoke | Build erfüllt; `net-smoke` als Root mit elf bestandenen Checks erfüllt |
| 5. Testmatrix, Quellen, Commit-ID, Logs, Lücken | Matrix/Quellen/Logs vorhanden; Commit-ID und Requirement-Abdeckung maschinenlesbar in `target/g1-report.json`; `docs/requirements.json` enthält den geprüften Status je F-/N-Anforderung |

**Ergebnis:** G1 ist mit diesem Lauf erfüllt:

> **G1 erreicht – synthetisch verifiziert.** Der Rust-Protokollkern und die
> USB-/Netzwerk-Mocks bestehen die dokumentierten Konformitäts-, Integrations-
> und Fehlertests auf dem in `target/g1-report.json` verzeichneten Commit.
> Der arm64-macOS-Build und der
> privilegierte `feth`/BPF/`scutil`-Smoke-Test sind erfolgreich. Eine
> funktionierende Verbindung zu einem echten Android-Gerät ist damit nicht
> automatisch nachgewiesen; dieser Nachweis wird separat in
> [G2-VERIFICATION.md](G2-VERIFICATION.md) geführt.

## Offene Lücken

1. Realer Replug, Sleep/Wake und Gerätewechsel; die IOKit-Power-Hooks und der
   Reconnect-Supervisor sind implementiert und synthetisch getestet, aber
   noch nicht in Hardware-Wiederholungen ausgeführt.
2. NCM/ECM-Delegation auf realer Hardware.
3. Line-Coverage liegt bei rund 61 %; Root-/Hardware-Pfade sind nur teilweise
   ausgeführt, Branch-Coverage wird von LLVM auf diesem Target nicht geliefert.
4. Signierung und Notarisierung; `uninstall` wurde noch nicht separat geprüft.
5. G2-Vollständigkeit: Modell-/Android-Version, Replug/Sleep/Wake je 3×,
   Durchsatzoptimierung.

## G2

G2 ist für die Kriterien 1–3 und den Cleanup-Teil von Kriterium 5 mit einem
realen Samsung-Gerät belegt. Kriterium 4 (Replug/Sleep/Wake-Wiederholungen)
und die Modell-/Android-Version sind noch offen. Protokoll:
[G2-VERIFICATION.md](G2-VERIFICATION.md).
