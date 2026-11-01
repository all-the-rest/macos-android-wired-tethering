# Android USB Tethering für macOS (RNDIS-Prototyp)

Ein Userspace-Prototyp für Android-USB-Tethering auf Apple Silicon. Das
Telefon wird als RNDIS-Gerät angesprochen; auf der macOS-Seite entsteht ein
echtes `feth`-Ethernet-Paar, dessen Host-Seite als normale macOS-Netzwerk­dienst-
Verbindung mit DHCP, Routen und DNS betrieben wird. Es gibt keinen Kernel-
Extension, keine SIP-Änderung und keinen DriverKit-Entitlement-Antrag.

> **Status:** Der Prototyp wurde am 1. November 2026 mit einem realen
> Samsung-Android-Gerät (USB `04e8:6863`, RNDIS) über USB-Tethering getestet.
> DHCP, DNS, Default Route, ICMP, HTTPS sowie ein 1-MB-Download und ein
> 1-MB-Upload funktionierten, auch **ohne WLAN-Uplink am Mac**. Der
> privilegierte macOS-Smoke-Test (`net-smoke`) besteht; der Helper läuft als
> LaunchDaemon. Einzelheiten und Rohdaten:
> [docs/G2-VERIFICATION.md](docs/G2-VERIFICATION.md). Die exakte
> Modell-/Android-Version ist über die RNDIS-only-USB-Konfiguration nicht
> ablesbar; der 15-Minuten-Lauf ist bestanden, Replug- und Sleep/Wake-
> Wiederholungen stehen noch aus.

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
`/Library/LaunchDaemons/all-the-rest.android-tether.helper.plist` und lädt sie mit
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

# RNDIS-Verbindung im Vordergrund starten (Standard: Priorität 2,
# also hinter dem aktuellen Default-Uplink)
android-tether connect
android-tether connect --device 1:5 --mtu 1500
# Tether ausdrücklich als primären Uplink verwenden, auch wenn WLAN aktiv bleibt
android-tether connect --prefer
# Nach einem Replug bis zu 120 s auf ein RNDIS-Gerät warten (0 = aus)
android-tether connect --reconnect-timeout 120

# Automatik als Benutzer-LaunchAgent: verbindet beim Anstecken und nach
# einem Replug selbstständig, ohne Root für den Agenten
android-tether install-agent
android-tether status
android-tether uninstall-agent

# Status / Beenden / Aufräumen
android-tether status --json
# status zeigt zusätzlich die von macOS per DHCP gesetzte IPv4-Adresse, sobald vorhanden
android-tether disconnect
android-tether cleanup
```

Beim Start werden nur die RNDIS-Control- und -Data-Interfaces beansprucht.
Andere Interfaces (ADB, MTP, Laden) bleiben unangetastet. Ist ein Interface
bereits von einem nativen Treiber belegt, bricht der Prototyp mit `usb_busy`
ab, statt den Treiber zu entfernen.

`install-agent` richtet einen LaunchAgent für den angemeldeten Benutzer ein.
Der Agent wartet dauerhaft auf ein RNDIS-Gerät, verbindet beim Anstecken
automatisch, räumt beim Abziehen auf und versucht es nach einem Replug erneut.
Er läuft unprivilegiert; nur der separat installierte Helper ist Root.
`disconnect` beendet die aktuelle Verbindung, der Agent startet sie bei
angeschlossenem Gerät jedoch wieder. Zum dauerhaften Abschalten
`android-tether uninstall-agent` verwenden.

### Synthetische Tests ohne Telefon

```bash
# Kompletter RNDIS-Handshake + bidirektionale ARP-/DHCP-Frames, keine Hardware
android-tether simulate --scenario basic
android-tether simulate --scenario dhcp --json
android-tether simulate --scenario faults
android-tether simulate --scenario reconnect

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
- Eintrag im aktuellen Netzwerk-Set. Standardmäßig wird der Dienst direkt
  **hinter den aktuellen Default-Uplink** gesetzt (Priorität 2): WLAN oder
  Ethernet bleibt primär, solange es eine Default Route hat; ohne andere
  Default Route rückt der Tether nach vorne. Mit `connect --prefer` wird er
  stattdessen primärer Uplink, auch wenn WLAN aktiv bleibt. Beim Beenden
  verschwindet der Dienst und die vorherige Reihenfolge gilt wieder.
- `feth` bleibt bis nach dem erfolgreichen RNDIS-Handshake administrativ down;
  erst dann wird der Link up geschaltet, damit macOS DHCP nicht verpasst.

Beim Beenden entfernt der Helper den Dienst und das feth-Paar. Die feste UUID
erlaubt `cleanup` auch nach einem abgestürzten Prozess, ohne fremde Dienste
anzufassen. Ist die Zustandsdatei verloren gegangen, bittet `cleanup` den
laufenden Helper zumindest um das Entfernen des reservierten Dienstes; ein
feth-Paar kann ohne gespeicherte Namen nicht sicher zugeordnet werden.

## Sicherheit und Datenschutz

- USB-Deskriptoren und RNDIS-Nachrichten werden als nicht vertrauenswürdige
  Eingaben behandelt: Längen, Offsets, Alignment und Überläufe werden geprüft.
- Der Root-Helper kennt nur feste Operationen (`CREATE`, `LINK`, `PREFER`,
  `FALLBACK`, `DESTROY`, `PING`, `STATUS`); er nimmt keine beliebigen
  Interface- oder Dienstnamen an.
- Der Unix-Socket ist auf den angegebenen Benutzer beschränkt; der Helper prüft
  zusätzlich die Peer-UID via `getpeereid`.
- Der automatische Agent läuft als angemeldeter Benutzer, nicht als Root.
- Logs enthalten keine Paketnutzdaten, Zugangsdaten oder Seriennummern.
  Gerätekennungen werden auf Bus/Adresse und VID/PID reduziert.
- Keine Telemetrie, keine Cloud-Abhängigkeit.

## Bewusste Grenzen des Prototyps

- **Nur RNDIS** wird aktiv implementiert. CDC-NCM und CDC-ECM werden erkannt und
  als native Delegation gemeldet; sie werden nicht vom eigenen Stack gefahren.
- Der USB-Datenpfad ist weiterhin synchron und single-threaded, aber der Hot
  Path pollt Benachrichtigungen nur noch alle 100 ms, wartet nicht mehr auf
  leere BPF-Frames und bündelt Frames pro RNDIS-Transfer. Die Messung nach
  dieser Optimierung ergab 1,82 MB/s (1 MB), 7,35 MB/s (5 MB) Download und
  1,18 MB/s Upload bei ca. 45 ms Ping; Twitch lief flüssig. Ein gepipeliner
  Datenpfad bleibt die nächste Ausbaustufe.
- Hotplug/Replug wird durch den Reconnect-Supervisor (`--reconnect-timeout`,
  Standard 60 s) behandelt: feth/BPF/Dienst bleiben bestehen, die Host-MAC
  wird gepinnt. Sleep/Wake ist über IOKit-Power-Benachrichtigungen angebunden
  (Link down bei `WillSleep`, RESET/Reinitialisierung bei `HasPoweredOn`);
  beides ist synthetisch und auf Hardware getestet (3× Replug, 1× Sleep/Wake).
- IPv6 wird als Ethernet weitergeleitet, aber nicht separat verifiziert.
- Die macOS-Integration (`feth`, BPF, `scutil`-Dienst) wurde auf diesem Mac als
  Root ausgeführt. `android-tether net-smoke --json` besteht alle elf Checks
  (feth-Paar, SCM_RIGHTS-BPF, Link up/down, Dienst, Priorität 2, Cleanup).
- `android-tether-helper install/uninstall` wurde als Root ausgeführt: Der
  Helper läuft als LaunchDaemon; `uninstall` entfernt Daemon, plist, Binary,
  Socket und den reservierten Dienst, und `net-smoke` besteht nach der
  Neuinstallation 11/11. Der automatische LaunchAgent ist installiert und
  verbindet das Gerät beim Anstecken.

## Signierung und Verteilung

CLI und Helper werden **ad-hoc** signiert (`codesign --force --sign -`).
`install-agent` signiert die kopierte CLI vor dem atomaren Austausch; der
Helper wird beim Bauen linker-/ad-hoc-signiert und beim Installieren
unverändert nach `/Library/PrivilegedHelperTools` kopiert.

Für andere Macs sind damit keine Developer ID und keine Notarisierung nötig.
macOS verlangt beim ersten Start eine einmalige Freigabe (Rechtsklick →
Öffnen, Systemeinstellungen → Datenschutz & Sicherheit → „Dennoch öffnen“,
oder bei CLI-Binaries `xattr -d com.apple.quarantine <pfad>`).

Ein Tag `v*` löst `.github/workflows/release.yml` aus: CI prüft Formatierung,
Clippy und Tests, baut arm64, signiert beide Binaries ad-hoc und hängt
`android-tether-v<version>-macos-arm64.tar.gz` samt `SHA256SUMS` an ein
GitHub-Release. Die Verteilung braucht damit keinen lokalen Build mehr:

```bash
# Archiv und Prüfsumme aus dem Release laden, dann:
shasum -a 256 -c SHA256SUMS
tar -xzf android-tether-v<version>-macos-arm64.tar.gz
cd android-tether-v<version>-macos-arm64
xattr -d com.apple.quarantine android-tether android-tether-helper 2>/dev/null

sudo ./android-tether-helper install --allow-uid "$(id -u)"
./android-tether install-agent
```

Der Quelltext-Build bleibt der Weg für eigene Änderungen und für Intel-Macs
(der Prototyp ist arm64-only).

## Tests und CI

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release --target aarch64-apple-darwin --workspace

# Coverage (benötigt cargo-llvm-cov + llvm-tools-preview)
cargo llvm-cov --workspace --json --summary-only \
  --output-path target/g1/coverage.json
```

`scripts/verify-g1.sh` führt Formatierung, Clippy, Tests, arm64-Build, Fuzzing,
Coverage und den (ggf. übersprungenen) macOS-Smoke-Test aus und schreibt
`target/g1-report.json`.

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
