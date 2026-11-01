# AGENTS.md — macos-android-wired-tethering

Rust-Prototyp für Android-USB-Tethering über RNDIS auf Apple-Silicon-macOS.
Kein Kernel-Extension, keine SIP-Änderung, keine DriverKit-Erweiterung.

## Sprache

Code und technische Kommentare auf Englisch; README und Prüfdokumente auf
Deutsch. Neue öffentliche Dokumentation folgt dem vorhandenen Ton.

## Architektur-Grenzen

- **Unprivilegiert:** USB/RNDIS, Ethernet-Frames, CLI, `auto`-Supervisor,
  LaunchAgent. Der Agent läuft als angemeldeter Benutzer.
- **Root-Helper:** nur die festen Operationen `CREATE`, `LINK`, `PREFER`,
  `FALLBACK`, `DESTROY`, `PING`, `STATUS`; keine beliebigen Interface- oder
  Dienstnamen.
- USB-Deskriptoren und RNDIS-Nachrichten sind nicht vertrauenswürdige Eingaben.
  Keine unkontrollierten `unsafe`-Blöcke; FFI-Grenzen dokumentieren.
- macOS-Integration: `feth`-Paar, BPF am Peer, `scutil --prefs` mit fester
  Dienst-UUID. Standard ist Priorität 2 (`FALLBACK`), `--prefer` ist die
  ausdrückliche Primärwahl.
- Keine echten Hardware- oder Root-Pfade in Unit-Tests; Hardware-Nachweise
  gehören nach `docs/G2-VERIFICATION.md`.

## Gates

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --release --target aarch64-apple-darwin --workspace
PATH="$HOME/.cargo/bin:$PATH" ./scripts/verify-g1.sh
ratchet check
```

`scripts/verify-g1.sh` schreibt `target/g1-report.json`; der macOS-Smoke-Test
braucht den laufenden Root-Helper und keine aktive Tether-Session.
`net-smoke` muss 11/11 Checks liefern. Der Helper wird mit
`android-tether-helper install --allow-uid "$(id -u)"` installiert, der
Benutzer-Agent mit `android-tether install-agent`.

## Definition of Done

- Verhaltensänderungen brauchen Tests; Hardware-/Root-Pfade zusätzlich einen
  dokumentierten Lauf oder eine klar benannte Lücke.
- Doku und Traceability aktualisieren: `README.md`, `docs/G2-VERIFICATION.md`,
  `docs/TRACEABILITY.md`, `docs/requirements.json`.
- Keine erfundenen Nachweise: nicht gemessene Performance und nicht geprüfte
  Gerätezustände bleiben ausdrücklich offen.

## CodeGraph

Der Index liegt unter `.codegraph/` und ist lokal/ignoriert. Der Pre-commit-Hook
`.githooks/pre-commit` synchronisiert ihn fail-open; nach dem Klonen einmalig
mit `git config core.hooksPath .githooks` aktivieren. Vor grep/find:
`codegraph explore "<Frage>"` oder `codegraph status`.

## Ratchet

`ratchet.config.jsonc` enthält die Fast-Tier-Gates; `ratchet.baseline.json` ist
maschinengeschrieben und wird nur mit
`ratchet baseline update` in erlaubter Richtung bewegt. CI und lokal:
`ratchet check`.

## Git und CI

Ein Commit pro Aufgabe, Conventional Commits. Es gibt ein GitHub-Remote unter
`all-the-rest`; die Workflow-Definition liegt in `.github/workflows/ci.yml`.
Vor dem Push müssen fmt, Clippy, Tests und der arm64-Build grün sein.
Hardware-Nachweise werden nicht durch CI ersetzt.

## Offene Arbeit

`AGENTS.todo.md` ist das Protokoll der offenen Arbeit; erledigte Einträge werden
gelöscht, nicht als `[x]` geführt. Die Historie steht in Git.
