# Android USB Tethering für macOS (RNDIS-Prototyp)

Ein Userspace-Prototyp für Android-USB-Tethering auf Apple Silicon. Das
Telefon wird als RNDIS-Gerät angesprochen; auf der macOS-Seite entsteht ein
echtes `feth`-Ethernet-Paar, dessen Host-Seite als normale macOS-Netzwerk­dienst-
Verbindung mit DHCP, Routen und DNS betrieben wird. Es gibt keinen Kernel-
Extension, keine SIP-Änderung und keinen DriverKit-Entitlement-Antrag.

> **Status:** Prototyp ohne Verifikation an einem echten Android-Gerät.
> Der synthetische Protokollkern, die Mock-USB-/Ethernet-Integration und der
> arm64-Build sind verifiziert. Ein Lauf gegen ein Samsung-Gerät (G2) ist
> ausdrücklich **nicht** durchgeführt. Der privilegierte macOS-Smoke-Test
> (`net-smoke`) benötigt den Root-Helper und wurde in dieser Umgebung nicht
> ausgeführt; siehe [docs/G1-VERIFICATION.md](docs/G1-VERIFICATION.md).

## Architektur

```text
┌──────────────┐   USB (libusb, synchron)   ┌────────────────────────────┐
│ Android      │ ◀────────────────────────▶ │ android-tether (Userspace) │
│ RNDIS-Gadget │  Control + Bulk Endpoints  │ RNDIS-Session, Frames      │
└──────────────┘                            └──────────────┬─────────────┘
                                                            │ BPF fd (SCM_RIGHTS)
                                            ┌───────────────▼─────────────┐
                                            │ android-tether-helper (root)│
                                            │ feth-Paar + BPF + scutil    │
                                            └───────────────┬─────────────┘
                                                            │
                                                    feth-Paar (Ethernet)
                                                            │
                                                   macOS Netzwerkdienst
                                                   DHCP · DNS · Routen
```

- **`tether-core`**: hardwareunabhängiger USB-Deskriptor-Parser, CDC-Klassifikation,
  RNDIS-Nachrichten-/Paket-Codec, Zustandsmaschine, Frame-Reassemblierung,
  Mock-USB-Gerät, Mock-Ethernet-Brücke und Engine.
- **`tether-usb`**: libusb-Transport (vendored libusb), Geräteerkennung ohne
  Interface-Claim. Nur RNDIS wird aktiv gefahren; NCM/ECM werden erkannt und an
  den nativen macOS-Treiber delegiert.
- **`tether-macos`**: `feth`-Paar, BPF-I/O, `scutil --prefs`-Netzwerkdienst,
  Unix-Socket-IPC mit `SCM_RIGHTS`-Dateideskriptor-Übergabe.
- **`tether-helper`**: schmaler Root-Helper. Er führt nur fest definierte
  feth-/BPF-/Netzwerkdienst-Operationen aus; der USB-/RNDIS-Prozess läuft
  unprivilegiert.
- **`tether-cli`**: `android-tether` mit `list`, `status`, `diagnose`,
  `connect`, `disconnect`, `cleanup`, `simulate`, `net-smoke`.

## Voraussetzungen

- macOS 13 oder neuer auf Apple Silicon (arm64)
- Rust 1.85+ (stable)
- Xcode Command Line Tools (für den vendored libusb-Build)
- Für echte Verbindungen: Root-Rechte **nur** für den Helper

## Bauen

```bash
cargo build --release --target aarch64-apple-darwin --workspace
file target/aarch64-apple-darwin/release/android-tether
file target/aarch64-apple-darwin/release/android-tether-helper
```

## Helper starten

Der Helper muss als Root laufen und darf nur den aufrufenden Benutzer über den
Unix-Socket zulassen. Für einen kurzen Test im Vordergrund:

```bash
sudo ./target/aarch64-apple-darwin/release/android-tether-helper serve \
  --allow-uid "$(id -u)"
```

Der Socket liegt standardmäßig unter
`/var/run/android-tether-helper.sock`, gehört dem angegebenen Benutzer und hat
Modus `0600`. Ein anderer Socket kann mit `--socket` (Helper) und
`--helper` (CLI, global) gewählt werden.

Für einen dauerhaften Start kann der Helper als LaunchDaemon installiert
werden. Der Befehl kopiert das Binary nach
`/Library/PrivilegedHelperTools/android-tether-helper`, schreibt die plist
`/Library/LaunchDaemons/io.github.android-tether.helper.plist` und lädt sie mit
`launchctl`:

```bash
sudo ./target/aarch64-apple-darwin/release/android-tether-helper install
# optional: --allow-uid 501 --binary ... --plist ... --socket ...

# Deinstallation (vorher 'android-tether cleanup' ausführen):
sudo ./target/aarch64-apple-darwin/release/android-tether-helper uninstall
```

Die Deinstallation entfernt plist, installiertes Binary, Socket und den
reservierten Netzwerkdienst. Ein noch vorhandenes feth-Paar wird nicht
blindlings gelöscht, weil feth-Namen nicht exklusiv diesem Projekt gehören;
`android-tether cleanup` entfernt es anhand des gespeicherten Zustands.

## Benutzung

```bash
# Geräte und erkannte Funktionen anzeigen (kein Root nötig)
android-tether list
android-tether list --all --json

# Umgebung, Helper, BPF und USB-Geräte prüfen
android-tether diagnose --json

# RNDIS-Verbindung im Vordergrund starten
android-tether connect
android-tether connect --device 1:5 --mtu 1500
# Nach einem Replug bis zu 120 s auf ein RNDIS-Gerät warten (0 = aus)
android-tether connect --reconnect-timeout 120

# Status / Beenden / Aufräumen
android-tether status --json
android-tether disconnect
android-tether cleanup
```

Beim Start werden nur die RNDIS-Control- und -Data-Interfaces beansprucht.
Andere Interfaces (ADB, MTP, Laden) bleiben unangetastet. Ist ein Interface
bereits von einem nativen Treiber belegt, bricht der Prototyp mit `usb_busy`
ab, statt den Treiber zu entfernen.

### Synthetische Tests ohne Telefon

```bash
# Kompletter RNDIS-Handshake + bidirektionale ARP-/DHCP-Frames, keine Hardware
android-tether simulate --scenario basic
android-tether simulate --scenario dhcp --json
android-tether simulate --scenario faults

# macOS feth/BPF/Netzwerkdienst-Smoke-Test (benötigt laufenden Root-Helper)
# Exit-Code 3 bedeutet "übersprungen", nicht "bestanden".
android-tether net-smoke --json
```

`simulate` ist **kein** Hardware-Nachweis. Es zeigt, dass der Protokollkern
einen vollständigen Handshake, Paketfilter, mehrere Frames pro Transfer,
ARP/DHCP-Testverkehr, Linkstatus und Fehler-Recovery korrekt verarbeitet.

### macOS-Netzwerkdienst

Der Helper legt über `scutil --prefs` einen Dienst mit fester UUID
`A0D4C0DE-7E7E-4A4A-8D8D-0000000A7070` an:

- `DeviceName = fethN`, `Hardware = Ethernet`, `Type = Ethernet`
- IPv4 `ConfigMethod = DHCP`
- Eintrag im aktuellen Netzwerk-Set; der Dienst wird **ans Ende** der
  Dienstreihenfolge gehängt, nicht nach vorne. Bei vorhandenem Uplink bleibt
  dessen Priorität erhalten.
- `feth` bleibt bis nach dem erfolgreichen RNDIS-Handshake administrativ down;
  erst dann wird der Link up geschaltet, damit macOS DHCP nicht verpasst.

Beim Beenden entfernt der Helper den Dienst und das feth-Paar. Die feste UUID
erlaubt `cleanup` auch nach einem abgestürzten Prozess, ohne fremde Dienste
anzufassen.

## Sicherheit und Datenschutz

- USB-Deskriptoren und RNDIS-Nachrichten werden als nicht vertrauenswürdige
  Eingaben behandelt: Längen, Offsets, Alignment und Überläufe werden geprüft.
- Der Root-Helper kennt nur feste Operationen (`CREATE`, `LINK`, `DESTROY`,
  `PING`, `STATUS`); er nimmt keine beliebigen Interface- oder Dienstnamen an.
- Der Unix-Socket ist auf den angegebenen Benutzer beschränkt; der Helper prüft
  zusätzlich die Peer-UID via `getpeereid`.
- Logs enthalten keine Paketnutzdaten, Zugangsdaten oder Seriennummern.
  Gerätekennungen werden auf Bus/Adresse und VID/PID reduziert.
- Keine Telemetrie, keine Cloud-Abhängigkeit.

## Bewusste Grenzen des Prototyps

- **Nur RNDIS** wird aktiv implementiert. CDC-NCM und CDC-ECM werden erkannt und
  als native Delegation gemeldet; sie werden nicht vom eigenen Stack gefahren.
- Der USB-Datenpfad ist synchron und single-threaded. Das ist für Korrektheit
  und den synthetischen Nachweis ausreichend, aber kein Durchsatzoptimum.
- Hotplug, Sleep/Wake und automatische Wiederverbindung nach Abziehen sind nur
  teilweise behandelt; die Engine hat begrenzte Reset-/Reconnect-Versuche.
- IPv6 wird als Ethernet weitergeleitet, aber nicht separat verifiziert.
- Die macOS-Integration (`feth`, BPF, `scutil`-Dienst) ist in dieser Umgebung
  nicht als Root ausgeführt worden. Der entsprechende `net-smoke` ist
  implementiert, aber hier mit Status **skipped** dokumentiert.
- Signierung, Notarisierung, launchd-Installation und eine öffentliche
  Verteilung sind nicht Teil dieses Prototyps.

## Tests und CI

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release --target aarch64-apple-darwin --workspace
```

Fuzzing (nightly + cargo-fuzz):

```bash
cargo +nightly fuzz build
cargo +nightly fuzz run descriptors -- -max_total_time=10
cargo +nightly fuzz run rndis -- -max_total_time=10
cargo +nightly fuzz run ethernet -- -max_total_time=10
```

Die Testmatrix, Quellenbezüge und der aktuelle Nachweisstand stehen in
[docs/TRACEABILITY.md](docs/TRACEABILITY.md) und
[docs/G1-VERIFICATION.md](docs/G1-VERIFICATION.md). Die technischen
Entscheidungen und Alternativen sind in [docs/TECH-SPIKE.md](docs/TECH-SPIKE.md)
dokumentiert; die RNDIS-Details in [docs/PROTOCOL.md](docs/PROTOCOL.md).

## Lizenz

MIT. Es wurde kein Code aus GPL-Projekten wie HoRNDIS oder Linux übernommen.
Die Implementierung orientiert sich an den in `PLAN.md` genannten
Spezifikationen; Betriebssystem-Mechanismen (`feth`, BPF, `scutil`) sind in
`docs/TECH-SPIKE.md` offengelegt.
