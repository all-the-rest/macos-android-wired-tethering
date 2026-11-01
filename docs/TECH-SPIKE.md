# Technologie-Spike: USB-Zugriff und macOS-Integration

Dieser Spike dokumentiert die im Prototyp getroffenen Entscheidungen. Er ist
kein Ersatz für die normativen Protokolldokumente aus `PLAN.md`; er beschreibt
ausschließlich die Transport- und Betriebssystemmechanismen.

## USB-Zugriff

**Entscheidung:** libusb über das `rusb`-Crate, mit der `vendored`-Funktion.
libusb wird beim Build aus dem mitgelieferten Quelltext gebaut; dadurch gibt es
keine Abhängigkeit von einem per Homebrew installierten libusb und der
arm64-Build ist reproduzierbar. Der Protokollkern kennt libusb nicht, sondern
spricht nur `tether_core::transport::UsbTransport`.

**Konsequenzen:**

- synchrone Control-/Bulk-/Interrupt-Transfers mit Timeouts;
- ein Transfer pro Richtung, kein asynchroner Ring; für den Korrektheits-
  Prototyp ausreichend, für Durchsatz später ersetzbar;
- Interface-Claims werden von libusb verwaltet; `kernel_driver_active` wird
  vor dem Claim geprüft, ein Detach findet nicht statt;
- nur Control- und Data-Interface werden beansprucht, andere Interfaces
  bleiben unangetastet.

**Nicht gewählt:** direkte IOKit-FFI. Sie würde mehr unsafe-Code und
IOUSBInterfaceInterface-Lifecycle-Verwaltung erfordern, ohne für den
synthetischen Nachweis einen Vorteil zu bringen. libusb ist auf macOS
verfügbar und erprobt.

## macOS-Ethernet-Brücke: `feth` + BPF

macOS enthält seit 10.13 ein undokumentiertes „fake Ethernet“-Paar (`feth`).
Zwei `feth`-Interfaces werden über `ifconfig ... peer ...` gekoppelt; Frames,
die auf der einen Seite eingespeist werden, erscheinen auf der anderen. Die
Host-Seite kann als normales Ethernet-Interface von der macOS-Netzwerk-
konfiguration verwendet werden.

**Erzeugung:**

```text
ifconfig feth<peer> create
ifconfig feth<host> create
ifconfig feth<host> lladdr <host-mac>
ifconfig feth<peer> peer feth<host>
ifconfig feth<peer> mtu <mtu> up
ifconfig feth<host> mtu <mtu> down
```

Die Host-Seite bleibt absichtlich down, bis der RNDIS-Handshake erfolgreich
war. Erst dann startet macOS DHCP; ein vorher verlorenes DHCP-Paket würde sonst
zu einer langen Link-Local-Wartezeit führen.

**Frame-I/O:** BPF (`/dev/bpfN`) auf der Peer-Seite.

- `BIOCSBLEN` 256 KiB, `BIOCIMMEDIATE=1`, `BIOCSSEESENT=0`,
  `BIOCSHDRCMPLT=1`, `BIOCPROMISC=1`, `BIOCSETIF=<peer>`.
- Gelesen wird ein Puffer mit mehreren `bpf_hdr`-Records; `bh_hdrlen`,
  `bh_caplen` und `bh_datalen` werden validiert. Der reale macOS-Kernel
  meldet `bh_hdrlen = 18` (8-Byte-`BPF_TIMEVAL`, zwei 32-Bit-Längen und eine
  16-Bit-Kopfzeilenlänge, gepackt), nicht die 20 Byte von
  `sizeof(struct bpf_hdr)` in Userspace. Der Parser akzeptiert 18–64 Byte und
  nutzt `bh_hdrlen` als Datenoffset.
- Geschrieben wird ein roher Ethernet-Frame. Die BPF-Schreibgrenze von ca.
  2048 Byte liegt über der hier verwendeten MTU von 1500, daher ist der
  zusätzliche AF_NDRV-Injektionsweg nicht nötig. AF_NDRV wäre nur für Jumbo-
  Frames erforderlich.
- `feth` ist undokumentiert. Das ist ein bewusst akzeptiertes Prototyp-Risiko;
  ein DriverKit-Ersatz ist in `PLAN.md` als separate Machbarkeitsfrage
  beschrieben.

## Sleep/Wake: IOKit-Power-Benachrichtigungen

`command_connect` registriert sich über `IORegisterForSystemPower` für
System-Power-Events. Die Callbacks laufen auf einer globalen Dispatch-Queue und
setzen atomare Flags:

- `kIOMessageSystemWillSleep`: Host-Link wird down geschaltet; der Callback
  quittiert mit `IOAllowPowerChange`.
- `kIOMessageSystemHasPoweredOn`: die Engine führt RESET und vollständige
  RNDIS-Reinitialisierung aus. Schlägt das fehl, übernimmt der Reconnect-
  Supervisor.
- `kIOMessageCanSystemSleep` wird nur quittiert, ohne den Link zu ändern.

Die Registrierung benötigt keine Root-Rechte. Der Monitor wird beim Beenden
wieder abgemeldet (`IODeregisterForSystemPower`,
`IONotificationPortDestroy`, `IOServiceClose`). Die FFI-Grenze liegt
ausschließlich in `tether-macos/src/power.rs`.

## Netzwerkdienst: `scutil --prefs`

`networksetup -createnetworkservice <name> <hardware-port>` funktioniert für
`feth` nicht, weil `feth` keinen IOKit-Hardware-Port besitzt. Der Prototyp
schreibt deshalb die SystemConfiguration-Präferenzen direkt über
`scutil --prefs`:

- fester Dienst-UUID `A0D4C0DE-7E7E-4A4A-8D8D-0000000A7070`
- `Interface = { DeviceName: fethN, Hardware: Ethernet, Type: Ethernet }`
- `IPv4 = { ConfigMethod: DHCP }`
- Eintrag im aktuellen `/Sets/<set>/Network/Service/<uuid>`
- Dienst wird an das Ende der `ServiceOrder` gehängt
- `commit` und `apply`, mit drei Versuchen bei parallel geänderten
  Präferenzen

Damit übernimmt macOS DHCP, Router, DNS und die Primärinterface-Entscheidung.
Der Prototyp ändert die Dienstreihenfolge nicht zugunsten des Tethering-
Dienstes; bei einem aktiven anderen Uplink bleibt dieser bevorzugt. Der
Dienst wird beim Beenden und bei Socket-Verlust des Helpers entfernt.

**Warum nicht SystemConfiguration-FFI?** Der Präferenzbaum ist über
`scutil --prefs` auditierbar und in einem schmalen Root-Helper gekapselt. Eine
große CoreFoundation-FFI-Fläche wäre für einen Prototyp schwerer zu prüfen.
Die feste UUID macht Cleanup deterministisch.

## Rechteaufteilung

- **Unprivilegiert:** USB-Deskriptoren lesen, RNDIS sprechen, Ethernet-Frames
  verarbeiten, Status anzeigen.
- **Root-Helper:** ausschließlich `feth`-Paar erzeugen/zerstören, Link
  up/down, BPF öffnen/konfigurieren, Netzwerkdienst über `scutil` anlegen/
  entfernen.
- Der Helper gibt den konfigurierten BPF-Fd per `SCM_RIGHTS` an den
  unprivilegierten Prozess. Der Helper behält keine Kopie.
- Socket: `0600`, Eigentümer ist die per `--allow-uid` freigegebene UID;
  zusätzlich `getpeereid`-Prüfung.

## Alternativen und bestehende Lösungen

| Ansatz | Bewertung |
|---|---|
| HoRNDIS (Kext) | Ältere Kext-Lösung; auf aktuellen Apple-Silicon-Systemen mit reduzierter Sicherheit problematisch bis nicht ladbar. GPL-Code wurde nicht übernommen. |
| TetherKit / usb-android-tethering | Existierende Userspace-Ansätze mit ähnlichem feth/BPF-Konzept. Sie wurden als Plausibilisierung gelesen, aber nicht kopiert; Paketformate stammen aus den Spezifikationen. Lizenzen wurden nicht übernommen. |
| Linux `rndis_host` | Sehr nützliche Verhaltensreferenz (Polling, Filter-Fallbacks, Offset-Basis), aber GPL; keine Codeübernahme. |
| DriverKit (`USBDriverKit` + `NetworkingDriverKit`) | Technisch der „richtige“ langfristige Weg, aber C++-ABI, Signierung, Entitlements und hohe Einstiegshürde. Für diesen Prototyp nicht verifiziert und nicht implementiert. |
| `utun` | Nur Layer 3; kein ARP/Ethernet, daher für eine echte macOS-Netzwerkdienst-Integration ungeeignet. |
| `vmnet` | Apple-Framework für virtuelle Netzwerke, aber an Virtualisierungs-Szenarien gebunden und nicht als genereller Tethering-Treiber gedacht. |

## Risiken und offene Punkte

1. `feth` ist undokumentiert und kann sich zwischen macOS-Versionen ändern.
2. BPF-Schreibgröße und `feth`-Verhalten bei hoher Last sind nicht gemessen.
3. `scutil --prefs`-Schema ist nicht als stabile öffentliche API garantiert;
   der Dienst wird deshalb mit fester UUID und ausschließlich eigenen Namen
   angelegt/entfernt.
4. Der Root-Helper ist ein Prototyp; eine launchd-Installation, Signierung und
   Notarisierung fehlen.
5. Ohne echtes Gerät sind RNDIS-Handshake, DHCP und Datenpfad nur synthetisch
   nachgewiesen.
