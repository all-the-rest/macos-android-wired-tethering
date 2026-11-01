# G2-Verifikation: reales Samsung-Gerät über USB

Stand: 1. November 2026. Dieses Protokoll dokumentiert den tatsächlich
ausgeführten Lauf mit einem angeschlossenen Android-Gerät. Es ist **kein**
Nachweis für alle Android-Modelle und noch kein vollständiges G2 nach
`PLAN.md` §7: Die Kriterien 1–3 sind belegt, der 15-Minuten-Lauf aus
Kriterium 4 ist bestanden. Replug-/Sleep-Wake-Wiederholungen und die
Modell-/Android-Version stehen noch aus.

## Ergebnis in einem Satz

Auf einem MacBook Pro (Apple <soc>, macOS 27.2, SIP aktiv) funktioniert
USB-Tethering mit einem Samsung-Android-Gerät über **RNDIS**: DHCP, DNS,
Default Route, ICMP und HTTPS laufen ohne WLAN-Uplink am Mac über das Telefon;
1 MB Upload und 1 MB Download waren erfolgreich. Mit `connect --prefer` wird
das Telefon auch bei eingeschaltetem WLAN zum primären Uplink, während die
WLAN-Verbindung aktiv bleibt. Der privilegierte `feth`/BPF/`scutil`-Smoke-Test
besteht.

## Umgebung

| Feld | Wert |
|---|---|
| Mac | MacBook Pro `<mac-model>`, Apple <soc>, <ram> |
| macOS | 27.2 (Build <build>), Darwin 27.2.0 arm64 |
| SIP | `System Integrity Protection status: enabled` |
| Authenticated Root | `enabled` |
| Rust | rustc/cargo 1.99.1, Ziel `aarch64-apple-darwin` |
| Build | Release, `cargo build --release --target aarch64-apple-darwin --workspace` |
| Helper | LaunchDaemon `all-the-rest.android-tether.helper`, läuft als Root |
| Commit | HEAD des Fix-Commits; exakter Wert in `target/g1-report.json` und `git log -1` |

## Gerät

| Feld | Wert |
|---|---|
| USB-Produkt | `SAMSUNG_Android` |
| Vendor/Product | `04e8:6863` (Samsung) |
| USB-Geschwindigkeit | High Speed (480 Mbit/s), `bcdUSB` 0x0201 |
| Konfiguration | 1, RNDIS Communications Control (Interface 0) + RNDIS Ethernet Data (Interface 1) |
| Endpunkte | Control-Interrupt `0x82`, Bulk IN `0x81`, Bulk OUT `0x01` |
| Seriennummer | vorhanden, aus Datenschutzgründen hier nicht dokumentiert |
| Modell / Android-Version | über die RNDIS-only-Konfiguration nicht ablesbar (kein ADB-Interface aktiv); offener Punkt |

Das Gerät wird über Deskriptoren als RNDIS erkannt, nicht über eine feste
VID/PID-Liste. `android-tether list --all --json` meldet:

```json
{
  "address": 2,
  "bus": 1,
  "configurations": [{
    "functions": [{
      "details": {
        "bulk_in": 129,
        "bulk_out": 1,
        "control_alt": 0,
        "control_class": "e0/01/03",
        "control_interface": 0,
        "data_alt": 0,
        "data_interface": 1,
        "interrupt_endpoint": 130,
        "max_segment_size": null
      },
      "protocol": "RNDIS",
      "reason": "RNDIS implementation available",
      "support": "usable"
    }],
    "value": 1
  }],
  "descriptor_error": null,
  "product_id": "6863",
  "speed": "high",
  "vendor_id": "04e8"
}
```

## Durchgeführte Nachweise

### 1. Privilegierter Helper und macOS-Smoke

Der Helper wurde mit `android-tether-helper install --allow-uid 501` als
LaunchDaemon installiert und gestartet:

```text
launchctl print system/all-the-rest.android-tether.helper
  state = running
  program = /Library/PrivilegedHelperTools/android-tether-helper
  properties = keepalive | runatload
/var/run/android-tether-helper.sock  srw------- <user>:daemon
```

`android-tether net-smoke --json` (Exit 0, `passed=true`, `skipped=false`):

| Check | Ergebnis |
|---|---|
| `feth_pair` | `feth100 <-> feth1100` |
| `bpf_descriptor` | via `SCM_RIGHTS` übergeben und nutzbar |
| `host_interface` | vorhanden |
| `link_up` / `link_state` | ok / Interface meldet UP |
| `link_down` | ok |
| `network_service` | während der Session vorhanden |
| `cleanup` | Interface wieder entfernt |

Rohdaten: `target/g1/net-smoke.json`.

### 2. Verbindung und DHCP

`android-tether connect` meldet nach dem RNDIS-Handshake:

```json
{
  "connected": true,
  "protocol": "RNDIS",
  "host_iface": "feth100",
  "peer_iface": "feth1100",
  "service": "Android USB Tethering (tetherd)",
  "host_mac": "<host-mac>",
  "mtu": 1486,
  "ipv4": "<tether-ip>",
  "last_error": null
}
```

Der macOS-Netzwerkdienst bezieht per DHCP:

| Feld | Wert |
|---|---|
| Adresse | `<tether-ip>/24` |
| Router | `<tether-gateway>` |
| DNS | `<tether-gateway>` |
| Lease | 3599 s, T1 1799 s, T2 3149 s |
| Vendor | `ANDROID_METERED` |

Der Dienst wird ohne Option **ans Ende** der Dienstreihenfolge gehängt. Bei
aktivem WLAN bleibt WLAN die primäre Route; ohne WLAN übernimmt `feth100`.
Das ist die dokumentierte Koexistenz-Entscheidung, kein stilles Übernehmen
eines bestehenden Uplinks.

### 2a. Tether als primärer Uplink (`connect --prefer`)

`connect --prefer` setzt den reservierten Dienst über die feste
Helper-Operation `PREFER` an den Anfang der Dienstreihenfolge. Die
Reihenfolge wird nach dem Commit erneut gelesen; schlägt das fehl, bricht
`connect` ab und räumt die Session wieder auf. Beim Beenden wird der Dienst
entfernt, sodass die vorherige Reihenfolge ohne separaten Restore-Schritt
wieder gilt.

Der Hardware-Nachweis wurde am 1. November 2026 mit angestecktem Samsung
`04e8:6863` und **eingeschaltetem WLAN** geführt. `networksetup
-listnetworkserviceorder` führte den Tether auf Platz 1, vor USB-Ethernet,
Thunderbolt Bridge und Wi-Fi:

```text
(1) Android USB Tethering (tetherd), Device: feth100
(2) USB-Ethernet, Device: en7
(3) Thunderbolt Bridge, Device: bridge0
(4) Wi-Fi, Device: en0
(5) VPN
```

`route -n get default` wählte danach das Telefon, obwohl `en0` weiterhin eine
aktive Adresse (`<wifi-ip>`) hatte:

```text
gateway: <tether-gateway>
interface: feth100
      mtu: 1486
```

Ein normales `curl https://www.google.com` **ohne** `--interface` und ohne
Deaktivieren von WLAN lieferte HTTP 200 über `<tether-ip>`, `remote_ip`
`<google-ip>`; `https://ifconfig.me/ip` meldete die Mobilfunk-Adresse
`<mobile-public-ip>`. Ping auf `8.8.8.8` war 4/4 erfolgreich (0 % Verlust,
538–1527 ms). Ein 1-MB-Download und ein 1-MB-Upload über dieselbe Route
lieferten HTTP 200 (22,7 s bzw. 17,6 s). Rohdaten:
`target/g2-prefer/`.

Nach `disconnect` war der Dienst entfernt, die Default Route lag wieder auf
`en0` (`<wifi-gateway>`), und es blieben keine `feth`-Interfaces oder
Tether-Dienste zurück.

### 2b. Standard-Priorität 2 und automatischer Start

Ohne `--prefer` verwendet `connect` die feste Helper-Operation `FALLBACK`.
Der Helper liest die aktuelle Default-Route, sucht den zugehörigen Dienst
und setzt den Tether direkt dahinter. Bei Default-Route über `en0` bleibt
WLAN also primär, der Tether ist die zweite Wahl; existiert keine Default
Route, rückt er an die erste Stelle. `net-smoke` prüft diesen Pfad auf dem
echten System und besteht jetzt **11/11** Checks
(`fallback_priority`, `target/g1/net-smoke.json`).

Für den Dauerbetrieb ist ein Benutzer-LaunchAgent installiert:

```text
launchctl print gui/501/all-the-rest.android-tether.auto
  state = running
  program = ~/Library/Application Support/android-tether/bin/android-tether
  arguments = ... --helper /var/run/android-tether-helper.sock auto
  plist = ~/Library/LaunchAgents/all-the-rest.android-tether.auto.plist
```

Der Agent läuft unprivilegiert, wartet auf ein RNDIS-Gerät und startet
`connect` mit Priorität 2. Beim Abziehen räumt der Verbindungsprozess auf;
der Agent wartet auf den nächsten Replug. Im Log stand nach der Installation:

```text
auto: waiting for an RNDIS tethering device
auto: no RNDIS tethering device; waiting
```

Der tatsächliche Ansteck-/Replug-Lauf mit dem Telefon war zu diesem Zeitpunkt
noch offen, weil das Gerät abgezogen war. Der Agent ist installiert und
laufbereit; der Nachweis muss nach dem nächsten Anstecken ergänzt werden.

### 3. Betrieb ohne anderen Internet-Uplink (G2-Kriterium 3)

WLAN (`en0`) wurde für den Test abgeschaltet. Danach war `feth100` die einzige
IPv4-Schnittstelle mit Default Route:

```text
scutil --nwi
  feth100 : address <tether-ip>, reachable
  Network interfaces: feth100

route -n get default
  gateway: <tether-gateway>
  interface: feth100

scutil --dns
  nameserver[0] : <tether-gateway>
  if_index : 25 (feth100)
  reach    : Reachable, Directly Reachable Address
```

Ergebnisse (Rohdaten unter `target/g1/evidence/no-uplink/`):

| Test | Ergebnis |
|---|---|
| `dig @<tether-gateway> example.com` | `<cloudflare-ip>`, `<cloudflare-ip>` |
| `dscacheutil -q host -a name example.com` | aufgelöst über den Systemresolver |
| `ping -c 4 8.8.8.8` | 4/4 Pakete, 0 % Verlust, 50–140 ms |
| `curl https://www.google.com` **ohne** Interface-Bindung | HTTP 200, `remote_ip=<google-ip>` |
| `curl https://ifconfig.me/ip` | `<mobile-public-ip>` |
| WLAN danach wieder ein | `en0` aktiv, Default Route wieder `<wifi-gateway>` |

Damit sind DNS, HTTPS und bidirektionaler Datentransfer über den Telefon-
Uplink belegt, während der Mac keinen anderen Internet-Uplink hatte.

### 4. Datentransfer

| Test | Ergebnis |
|---|---|
| 1 MB Upload zu `https://speed.cloudflare.com/__up` | HTTP 200, 1.000.000 Bytes, 17,6 s (~57 KB/s) |
| 1 MB Download von `https://speed.cloudflare.com/__down?bytes=1000000` | HTTP 200, 1.000.000 Bytes, 22,7 s (~44 KB/s) |
| 10 MB Download | nach 60 s abgebrochen (2,7 MB), kein Fehler der Verbindung |

Der Durchsatz ist für Twitch zu niedrig; auch der Ping lag im gleichen Lauf
bei 0,5–1,5 s. Der Datenpfad ist synchron und single-threaded. Der Hot Path
pollt Benachrichtigungen jetzt nur noch alle 100 ms, wartet nicht mehr auf
leere BPF-Frames und bündelt Frames in einem RNDIS-Transfer. Eine neue
Messung nach dieser Optimierung steht aus, weil das Gerät beim Test abgezogen
wurde. Das ist als offene Performance-Arbeit dokumentiert, nicht als
Korrektheitsfehler.

### 5. Langzeit- und Fehlerfälle

| Test | Status |
|---|---|
| 15-Minuten-Lauf mit Ping und HTTPS alle 15/60 s | **bestanden**: 905 s, 58 Pings, 0 Ping-Fehler, 16 HTTPS-Anfragen, 0 HTTPS-Fehler (`target/g1/evidence/stability.log`) |
| `disconnect` | erfolgreich; `feth`-Paar, Dienst und State-Datei entfernt |
| erneutes `connect` nach Helper-Neuinstallation | erfolgreich; neue Session, gleiche Bridge, DHCP erneut bezogen |
| Replug, Tethering aus/an, Sleep/Wake (je 3×) | **noch offen** |
| `connect --prefer` mit aktivem WLAN | **bestanden**: `feth100` primär, HTTP 200 ohne Interface-Bindung, WLAN blieb aktiv |
| Standard-Priorität 2 (`FALLBACK`) | **bestanden** im `net-smoke`: Dienst hinter dem Default-Uplink einsortiert |
| Auto-Agent installiert und wartend | **bestanden**: LaunchAgent `running`, Log zeigt Warten auf RNDIS |
| Auto-Connect beim echten Anstecken | **noch offen** (Gerät beim Test abgezogen) |
| echter Gerätewechsel | **noch offen** |

### 6. Cleanup

Nach `android-tether disconnect`:

```text
android-tether status --json
  {"connected": false, "reason": "no state file"}

ifconfig -a | grep feth
  (keine Ausgabe)
networksetup -listallnetworkservices | grep -i tether
  (keine Ausgabe)
```

`net-smoke` prüft denselben Cleanup-Pfad zusätzlich mit einem eigenen
feth-Paar; alle zehn Checks bestehen.

## Gefundene und behobene Fehler aus dem realen Lauf

1. **CREATE-Antwort mit Leerzeichen im Dienstnamen**
   `crates/tether-cli/src/helper_client.rs` zerlegte die Antwort mit
   `split_whitespace()` und erwartete genau vier Felder. Der Default-Name
   `Android USB Tethering (tetherd)` enthält Leerzeichen, wodurch jede reale
   Verbindung mit `malformed CREATE response from helper` abbrach. Der Parser
   verwendet jetzt `splitn(4, ' ')` und behandelt den Rest als Dienstnamen;
   Regressionstests decken den realen Namen und unvollständige Antworten ab.

2. **Darwin-`bpf_hdr` ist 18 Bytes lang**
   `crates/tether-macos/src/bpf.rs` erwartete eine Kopfzeilenlänge von 20
   Bytes. Der macOS-Kernel meldet im realen BPF-Read `bh_hdrlen = 18`
   (8-Byte-`BPF_TIMEVAL`, zwei 32-Bit-Längen, 16-Bit-Kopfzeilenlänge, gepackt);
   dadurch schlug der erste empfangene Frame mit `invalid bpf_hdr length` fehl.
   Der Parser akzeptiert jetzt 18–64 Bytes, nutzt `bh_hdrlen` als Datenoffset
   und richtet den nächsten Record weiter auf 4 Bytes aus. Ein Test deckt
   zusätzlich die gepolsterte 20-Byte-Variante ab.

## Bewertung gegen PLAN.md §7 (G2)

| G2-Kriterium | Status |
|---|---|
| 1. Samsung erkannt, USB-Tethering aktiv, SIP normal | **erfüllt** (SIP enabled, Gerät `04e8:6863`, RNDIS) |
| 2. nutzbares Interface, DHCP/IP/Gateway/DNS/Route nachvollziehbar | **erfüllt** (`feth100`, `<tether-ip>`, Gateway/DNS `<tether-gateway>`) |
| 3. ohne anderen Uplink DNS/HTTPS/bidirektional | **erfüllt** (WLAN aus, HTTP 200, Ping 4/4, Upload/Download) |
| 4. 15 Minuten + Replug/Tethering/Sleep/Wake je 3× | **teilweise**: 15-Minuten-Lauf bestanden; Replug/Tethering/Sleep/Wake offen |
| 5. Cleanup ohne Reste | **erfüllt** für `disconnect` und `net-smoke`; `uninstall` noch nicht separat geprüft |
| 6. Prüfprotokoll mit Gerät/Version/Commit/IP/Routing/Resultaten | **teilweise**: Modell/Android-Version über RNDIS nicht ablesbar; Commit-ID steht in `target/g1-report.json` |

**Korrekte Aussage:** USB-Tethering funktioniert mit diesem konkreten
Samsung-Gerät über RNDIS auf diesem Mac. Das ist **kein** Nachweis für alle
Android-Modelle, für NCM/ECM-Hardware oder für Sleep/Wake/Replug.

## Offene Punkte

1. Samsung-Modell und Android-/One-UI-Version über ADB oder Telefon-UI
   erfassen und im Protokoll ergänzen.
2. Replug, Tethering aus/an und Sleep/Wake je dreimal auf Hardware testen.
3. Durchsatz optimieren (gepipelined USB-Reader/Writer, mehrere Transfers pro
   Iteration); aktuell ca. 53 KB/s Download.
4. NCM/ECM-Delegation an echte Hardware prüfen.
5. Signierung/Notarisierung und `uninstall`-Lauf.

## Rohdaten

- `target/g1/net-smoke.json`
- `target/g1/evidence/status-live.json`
- `target/g1/evidence/feth100.txt`
- `target/g1/evidence/dhcp.txt`
- `target/g1/evidence/routes.txt`
- `target/g1/evidence/dns-via-phone.txt`
- `target/g1/evidence/ping-gateway.txt`
- `target/g1/evidence/curl-https-google.txt`
- `target/g1/evidence/no-uplink/`
- `target/g1/evidence/upload-1mb.txt`
- `target/g1/evidence/download-1mb.txt`
- `target/g1/evidence/stability.log`
