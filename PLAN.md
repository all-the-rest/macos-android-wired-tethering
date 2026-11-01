# Anforderungsdokument: Android-USB-Tethering für macOS (Apple Silicon)

**Version:** 1.0 (Entwurf)  
**Stand:** 1. November 2026  
**Zielplattform:** macOS auf Apple Silicon (arm64), zunächst macOS 13 oder neuer  
**Programmiersprache:** vorzugsweise Rust  
**Projekttyp:** Open Source; konkrete Lizenz vor Übernahme fremden Codes prüfen

## 1. Projektziel und Grenzen

Ein Android-Smartphone soll bei aktiviertem USB-Tethering auf einem Apple-Silicon-Mac als Internetverbindung nutzbar sein, ohne WLAN-Hotspot, Jailbreak/Root am Smartphone, Kernel-Erweiterungen, deaktivierte System Integrity Protection (SIP) oder reduzierte macOS-Sicherheit.

Die Lösung soll Hersteller- und Modellabhängigkeiten möglichst durch Unterstützung offener bzw. dokumentierter USB-Netzwerkprotokolle vermeiden. **„Alle Android-Handys“ ist ein langfristiges Kompatibilitätsziel, keine vorab beweisbare Erfolgsgarantie.** Unterstützung kann nur für Geräte zugesichert werden, die ein implementiertes Protokoll korrekt anbieten und deren USB-Schnittstelle macOS zugänglich macht. Die erste reale Referenz ist ein vorhandenes Samsung-Smartphone.

Zwei **unabhängige Abnahmestufen** gelten: **G1: synthetisch, ohne Android-Gerät** und **G2: reale Verbindung mit Samsung-Gerät**. G1 darf nicht als Nachweis von G2 ausgegeben werden.

## 2. Architekturentscheidung

**Standardweg (präferiert):** Native, arm64-Rust-Userspace-Anwendung mit klar getrennten Modulen für USB-Transport, USB-Deskriptor-Erkennung, RNDIS-Protokoll, Ethernet-Frame-Brücke, Netzwerkstatus und Diagnose. Für USB kann eine erprobte macOS-kompatible Bibliothek wie libusb über Rust-FFI evaluiert werden; für die Systemintegration sind insbesondere ein `feth`-Paar/BPF oder eine gleichwertige, sauber in macOS integrierte Lösung zu prüfen. DHCP und DNS sollen möglichst über die macOS-Netzwerkdienste abgewickelt werden. Falls erhöhte Rechte nötig sind, nur für einen eng abgegrenzten Helper, nicht für die gesamte UI.

**Alternative:** Eine echte macOS DriverKit-Systemerweiterung mit `USBDriverKit` und `NetworkingDriverKit` (`IOUserNetworkEthernet`), installiert über eine Host-App. Apples DriverKit-API ist C++-orientiert; ein dünner C++-Adapter mit einem Rust-Protokollkern ist denkbar, aber zuerst in einem separaten technischen Spike auf ABI, unterstützte Toolchains, Linkbarkeit und Laufzeitbedingungen zu verifizieren. **Eine vollständig in Rust geschriebene DriverKit-Erweiterung darf nicht ungeprüft als machbar angenommen werden.** DriverKit erfordert Signierung und passende, gegebenenfalls erst zu genehmigende Entitlements; generisches klassenbasiertes Matching für viele fremde Hersteller ist als zusätzlicher Machbarkeitsnachweis zu behandeln.

**Entscheidungskriterium:** Zunächst die einfachste Lösung wählen, die auf einem Mac mit unveränderten Sicherheitsfunktionen eine stabile native Netzwerkverbindung herstellt. DriverKit ist keine Pflicht, wenn der Userspace-Weg das Ergebnis zuverlässig erreicht. Kein `utun`-Only-Prototyp darf ohne Nachweis korrekter Ethernet-/ARP-/DHCP-Integration als fertige Lösung gelten.

## 3. Funktionale Anforderungen

- **F-01 Geräteerkennung:** USB-Konfigurationen, Interface Association Descriptors, CDC-Control-/Data-Interfaces, Alternate Settings sowie Control-, Interrupt- und Bulk-Endpunkte erkennen. RNDIS, CDC-NCM und CDC-ECM unterscheiden. Geräte anhand von Deskriptoren/Protokollen statt einer festen Samsung-VID/PID-Liste erkennen. Nicht unterstützte Konfigurationen verständlich melden.
- **F-02 Koexistenz:** Nur die erforderlichen USB-Schnittstellen beanspruchen; ADB, MTP, Laden und andere Interfaces nach Möglichkeit unangetastet lassen. Bereits von macOS nativ unterstützte Netzwerkinterfaces nicht verdrängen.
- **F-03 RNDIS (MVP, verpflichtend):** USB-CDC-Mapping mit encapsulated commands/responses, Benachrichtigungen und Bulk-I/O; Initialisierung, Versions-/Größenaushandlung, Query/Set von relevanten OIDs, MAC-Adresse, Packet Filter, Ethernet-Paketkapselung/Entkapselung, Link-/Fehlerstatus, Keepalive, Reset, Halt, Timeouts und Wiederanlauf entsprechend den Referenzspezifikationen.
- **F-04 CDC-NCM (Erweiterung):** Erkennung und korrekte Delegation an einen funktionierenden Systemtreiber. Eigene Implementierung nur, wenn für weitere Geräte erforderlich; dann NCM-spezifische Control Requests sowie NTB-/NDP-Framing und Negotiation separat testen.
- **F-05 CDC-ECM (Erweiterung):** Vorhandenen macOS-Support bevorzugen; eigene Implementierung nur bei nachgewiesenem Bedarf.
- **F-06 Netzwerkintegration:** Ein für macOS nutzbares Netzwerkinterface mit stabiler Ethernet-Übertragung anbieten; IPv4-DHCP, DNS, Default Route, Linkstatus und MTU berücksichtigen. IPv6, sofern vom Telefon angeboten, korrekt behandeln oder explizit als nicht unterstützt melden.
- **F-07 Lebenszyklus:** USB-Tethering ein-/ausschalten, Abziehen/Anstecken, Schlaf/Wach, USB-Reset, Gerätetausch und Netzwerkwechsel ohne dauerhaft defekte Routen, blockierte Geräte oder hängenbleibende Hintergrundprozesse verarbeiten.
- **F-08 Diagnostik:** `list`, `status`, `diagnose` und optional `connect`/`disconnect`; Diagnose nennt erkannte Geräte-/Interface-Klassen, gewähltes Protokoll, Verbindungszustand, technische Fehlerkategorie und verwendete Netzwerkschnittstelle. Logs enthalten keine Nutzdaten, Zugangsdaten oder unnötigen Gerätekennungen.
- **F-09 Installation/Deinstallation:** Reproduzierbarer arm64-Build, sichere Aktivierung, vollständige Deinstallation und Rückbau eigener Netzwerk-Konfigurationen. Keine Aufforderung zum Deaktivieren von SIP.

## 4. Nichtfunktionale Anforderungen

- **N-01 Sicherheit:** USB-Deskriptoren und RNDIS-Nachrichten als nicht vertrauenswürdige Eingaben behandeln; Längen, Offsets, Alignment, Speicherbedarf und Zustandsübergänge validieren. Keine unkontrollierten `unsafe`-Blöcke; alle FFI-Grenzen dokumentieren.
- **N-02 Robustheit:** Keine unendlichen Retries ohne Backoff; Fehler dürfen nicht zu Abstürzen, Speicherlecks oder Ressourcenverlust führen. Wiederverbindung nach Fehlern automatisch, begrenzt und nachvollziehbar.
- **N-03 Testbarkeit:** Protokollkern ohne Hardware, Netzwerkrechte und Apple-spezifische APIs ausführbar. USB-Transport als austauschbares Trait/Interface mit deterministischem Mock; eindeutige, reproduzierbare Test-Fixtures und Seed-Werte.
- **N-04 Wartbarkeit:** Rust als Hauptsprache; externe C-/C++-Brücken nur mit belegtem technischen Grund. CI mit `cargo fmt`, `clippy`, `test`, statischer Prüfung, Fuzzing und arm64-macOS-Build.
- **N-05 Datenschutz:** Offline nutzbar; keinerlei Telemetriepflicht; Debug-Traces optional und datensparsam. Keine automatische Änderung globaler Netzwerkeinstellungen ohne dokumentierte Notwendigkeit.
- **N-06 Distribution:** Signing, Notarisierung und gegebenenfalls DriverKit-Entitlements vor einer öffentlichen Veröffentlichung gesondert nachweisen; sie sind keine Voraussetzung für synthetische Protokolltests.

## 5. Normative Quellen und Nachweisführung

| Kennung | Dokument / Quelle | Implementierungs- und Testbezug |
|---|---|---|
| S-01 | Microsoft [MS-RNDIS], Remote Network Driver Interface Specification: https://download.microsoft.com/download/5/0/1/501ED102-E53F-4CE0-AA6B-B0F93629DDC6/Windows/%5BMS-RNDIS%5D.pdf | RNDIS-Message-Layout, Host-Zustandsmaschine, Request/Completion-Paare, Timeouts, Packet Framing |
| S-02 | Microsoft, Remote NDIS to USB Mapping: https://learn.microsoft.com/en-us/windows-hardware/drivers/network/remote-ndis-to-usb-mapping | USB-Control/Data-Kanäle, Endpunkte und USB-Transport |
| S-03 | USB-IF, Class Definitions for Communication Devices (CDC), einschließlich ECM: https://www.usb.org/documents?search=CDC | CDC-Deskriptoren, Interface-Zuordnung, ECM |
| S-04 | USB-IF, Network Control Model (NCM) v1.1 (2026), über Dokumentbibliothek: https://www.usb.org/documents?type%5B%5D=55 | NCM-Deskriptoren, Negotiation, NTBs |
| S-05 | USB-IF, Defined Class Codes: https://www.usb.org/defined-class-codes | Protokollbasierte Klassifikation |
| S-06 | AOSP Tethering-Konfiguration: https://android.googlesource.com/platform/packages/modules/Connectivity/+/refs/heads/master/Tethering/res/values/config.xml | Android-RNDIS/NCM-Auswahl; kein Beweis für Herstellerkompatibilität |
| S-07 | Apple NetworkingDriverKit: https://developer.apple.com/documentation/networkingdriverkit | Echte Ethernet-Treiber-Alternative |
| S-08 | Apple USBDriverKit: https://developer.apple.com/documentation/usbdriverkit | USB-Zugriff im DriverKit-Fall |
| S-09 | Apple DriverKit-Entitlements: https://developer.apple.com/documentation/driverkit/requesting-entitlements-for-driverkit-development | Signierung, Berechtigungen, Machbarkeit der Veröffentlichung |

**Regel:** Jede normative Protokollanforderung und jeder Konformitätstest bekommt einen Quellenverweis bis hin zur relevanten Sektion. Keine Paketformate ausschließlich aus fremdem Projektcode ableiten. Projektcode dient zur Plausibilisierung, nicht als Ersatz für Spezifikationen.

**Implementierungsreferenzen, nicht normativ:** https://github.com/stlk0/rndis-tether ; https://github.com/prostec-labs/TetherKit ; https://github.com/jwise/HoRNDIS . Lizenzbedingungen bei Codeübernahme prüfen, insbesondere Copyleft bei HoRNDIS.

## 6. Testsuite und Traceability

| Testgruppe | Synthetische Tests (ohne Gerät) | Bezug |
|---|---|---|
| T-01 Deskriptoren | Gültige RNDIS-/NCM-/ECM-Varianten, zusammengesetzte Geräte, wechselnde Interface-Nummern, Alternate Settings, unvollständige/defekte Deskriptoren, Ablehnung fremder USB-Klassen | F-01, F-02; S-02 bis S-05 |
| T-02 RNDIS-Nachrichten | Golden Vectors für Initialize/Halt/Query/Set/Reset/Keepalive, Completion-IDs, Statuscodes, OID-Antworten, Endianness, Offsets, Padding | F-03; S-01 |
| T-03 Ethernet-Daten | Roundtrip von Ethernet-Frames inklusive Broadcast/ARP, mehrere Frames pro Transfer, Fragmentierung/kurze Transfers, korrekte Packet-Längen, Reihenfolge und Payload-Integrität | F-03, F-06; S-01, S-02 |
| T-04 Negativ-/Sicherheitstests | Trunkierte Nachrichten, inkonsistente Offsets/Längen, Overflow, übergroße Transfers, ungültige Zustände, fehlende Antworten, falsche Request-IDs, Fuzz-Corpus | N-01, N-03; S-01, S-02 |
| T-05 Zustandsmaschine | Simulierter kompletter USB-Handshake und RNDIS-Session, Paketfilter, Link-Down/Up, Timeout, Keepalive, Reset und Wiederverbindung | F-03, F-07; S-01, S-02 |
| T-06 Host-Brücke | Mock-USB-Gerät plus simulierte Ethernet-Gegenstelle: bidirektionale Frame-Weitergabe, ARP/DHCP-Testverkehr, Interface-Cleanup; macOS-Integration auf Mac ohne Smartphone als separater Smoke-Test | F-06, F-07; S-07 soweit relevant |
| T-07 Betrieb | Mehrfaches Start/Stop, parallele Pakete, Backpressure, Ressourcenfreigabe, wiederholtes Fehler-Recovery, verständliche Diagnosen | F-07 bis F-09; N-01 bis N-05 |
| T-08 NCM/ECM | Positive/negative Klassifikation und Delegation; falls eigener NCM-Stack: unabhängige NTB-/NDP-Fixtures sowie Fehlerfälle | F-04, F-05; S-03 bis S-05 |

Test-Fixtures dürfen nicht nur von der eigenen Encoder-Ausgabe stammen: zusätzliche unabhängig aus Spezifikationsfeldern konstruierte Golden Vectors und Negativfälle sind Pflicht. Ein Encoder/Decoder-Roundtrip allein beweist keine Konformität. CI muss Testanzahl, Fehlversuche, Fuzzing-Ergebnisse und Abdeckung der Muss-Anforderungen maschinenlesbar dokumentieren. Aussagen wie „100 % standardkonform“ nur bei nachgewiesener Abdeckung des explizit unterstützten Protokollumfangs.

## 7. Meilensteine und Abnahme

### G1: Synthetische Verifikation – ohne Smartphone

**Erreicht genau dann, wenn:**

1. `cargo test` (einschließlich Integrations- und Mock-USB-Tests) und sämtliche verpflichtenden CI-Prüfungen erfolgreich sind.
2. Eine simulierte RNDIS-Initialisierung bis zur bidirektionalen Ethernet-Frame-Übertragung durchläuft; fehlerhafte und unerwartete USB-/RNDIS-Antworten sauber behandelt werden.
3. Alle in Abschnitt 6 für das MVP einschlägigen Testgruppen automatisiert und reproduzierbar ausführbar sind, inklusive fuzzbasierter Robustheitsprüfung.
4. Ein arm64-macOS-Build erfolgreich ist und ein hardwarefreier macOS-Integrations-/Netzwerk-Smoke-Test soweit technisch möglich ausgeführt wurde; fehlt dieser, G1 nur als **Protokollkern verifiziert**, nicht als komplette synthetische Integration bezeichnen.
5. Protokoll-Testmatrix, konkrete Quellenabschnitte, Commit-ID, CI-Logs und verbleibende Lücken dokumentiert sind.

**Vorgegebene Abschlussmeldung:**

> **G1 erreicht – synthetisch verifiziert.** Der Rust-Protokollkern und die USB-/Netzwerk-Mocks bestehen die dokumentierten Konformitäts-, Integrations- und Fehlertests auf Commit `<SHA>`. Der arm64-macOS-Build ist erfolgreich. **Eine funktionierende Verbindung zu einem echten Android-Gerät ist damit ausdrücklich noch nicht nachgewiesen.** Offene Einschränkungen: `<Liste oder keine>`.

### G2: Hardware-Verifikation – Samsung über USB

**Voraussetzung:** G1 erreicht; ein konkretes Samsung-Modell mit Android-/One-UI-Version sowie ein Apple-Silicon-Mac mit macOS-Version sind im Testprotokoll vermerkt.

**Erreicht genau dann, wenn:**

1. Auf einem Mac mit aktiviertem SIP und normaler Sicherheitskonfiguration wird das Samsung über ein USB-Datenkabel erkannt, nachdem USB-Tethering am Telefon aktiviert wurde.
2. Ein nutzbares Netzwerkinterface bzw. eine regulär integrierte Netzwerkverbindung entsteht; DHCP/IP-Adresse, Gateway, DNS und Route sind nachvollziehbar. Keine bestehende USB-Funktion wird unnötig blockiert.
3. **Ohne WLAN, Ethernet oder einen anderen aktiven Internet-Uplink am Mac** funktionieren DNS-Auflösung, HTTPS über den Mobilfunk-Uplink und bidirektionaler Datentransfer. Bei IPv6 nur dann IPv6-Erfolg behaupten, wenn vom Telefon angeboten und tatsächlich nachgewiesen.
4. Mindestens 15 Minuten Betrieb mit kleinen, dokumentierten Download-/Upload-Tests ohne Verbindungsausfall; Abziehen/Anstecken, Tethering aus/an und Sleep/Wake funktionieren bei jeweils mindestens drei Wiederholungen.
5. Nach dem Beenden und nach der Deinstallation bleiben keine eigenen unerwünschten Routen, Interfaces oder Autostart-Dienste zurück; es sind keine Abstürze oder dauerhaft blockierten USB-Schnittstellen aufgetreten.
6. Prüfprotokoll enthält Geräte- und Versionsdaten, Commit-ID, verwendetes Protokoll, IP-/Routing-Nachweise, Testresultate sowie offene Einschränkungen. Ein einziges erfolgreiches Samsung-Gerät gilt **nicht** als Nachweis für alle Android-Modelle.

**Vorgegebene Abschlussmeldung:**

> **G2 erreicht – Live-Test erfolgreich.** Auf `<Mac/macOS>` funktioniert USB-Tethering mit `<Samsung-Modell/Android-Version>` über `<RNDIS oder NCM>`. DHCP, DNS, HTTPS und bidirektionaler Datentransfer wurden ohne weiteren Internet-Uplink erfolgreich geprüft; Wiederverbindung, Sleep/Wake und Cleanup wurden bestanden. Testprotokoll: `<Pfad/CI- oder Artefakt-Link>`. **Bestätigte Hardware-Kompatibilität: dieses getestete Modell; weitere Android-Geräte sind noch nicht verifiziert.**

## 8. Bewusst außerhalb des MVP

Eine grafische Oberfläche, automatische Updates, App-Store-Vertrieb, iPhone-Unterstützung, WLAN-/Bluetooth-Tethering, alle herstellerspezifischen USB-Modi, garantierte NCM-Implementierung und ein genereller Nachweis „alle Android-Geräte“ gehören nicht zur ersten Abnahme. Zusätzliche Geräte werden später über eine öffentliche, versionierte Kompatibilitätsmatrix und Hardware-Testberichte ergänzt.

## 9. Empfohlene erste Arbeitspakete

1. Einen dokumentierten Rust-Technologie-Spike für USB-Zugriff und macOS-`feth`/BPF-Brücke durchführen, inklusive Sicherheits- und Rechtebedarf. Parallel prüfen, ob vorhandene Lösungen die Anforderung bereits erfüllen.
2. Spezifikationen in eine maschinenlesbare Traceability-Tabelle übersetzen: Requirement-ID ↔ Dokument/Abschnitt ↔ Test-ID ↔ Implementierungsmodul.
3. Protokollunabhängige USB-Abstraktion, RNDIS-Zustandsmaschine und sichere Frame-Parser in Rust implementieren.
4. Mock-USB-Gerät, unabhängige Testvektoren, Fehlereinspielung und Fuzzing implementieren; G1 abnehmen.
5. Erst danach Samsung-Gerät für G2 anschließen; gefundene Varianten in reproduzierbare Regressionstests überführen.
