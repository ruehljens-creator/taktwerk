# Taktwerk

Offener **AES67-Node** — eine virtuelle Soundkarte (Audio-over-IP) auf Basis
offener Standards (AES67 / RAVENNA / ST-2110-30), Dante-frei und lizenzfrei.
Zwei zuschaltbare Rollen: **Router/Kreuzschiene** (Control-Plane) und
**PTP-Masterclock**. Bedienung über ein gemeinsames Web-Frontend.

Vollständige Spezifikation: [`Taktwerk_Projektbrief.md`](Taktwerk_Projektbrief.md).

> **Status:** Phase 0 — plattformneutraler Protokoll-/DSP-Kern. Headless
> (kein virtuelles Gerät), auf allen Ziel-OS baubar und testbar.

---

## Multiplattform von Tag 1

Ziel-OS (Produkt): **Linux + macOS** first-class; **Windows** kompiliert den
portablen Kern + Router mit (virtuelles Gerät später). Das Leitprinzip:

> **Der gesamte Protokoll-/DSP-Kern (`taktwerk-core`) ist OS-neutral und hängt
> von keiner Plattform-API ab.** Alles OS-Spezifische sitzt hinter Traits in
> eigenen Crates und wird per `cfg`/Feature nur auf der jeweiligen Plattform
> eingezogen. Ein Build auf einer OS zieht nie die Backends der anderen herein.

### Portabilitäts-Matrix

| Baustein | Linux | macOS (arm64) | Windows | Wo |
|---|:---:|:---:|:---:|---|
| RTP L24/L16, SDP, SAP, PTP-BMCA, ASRC-Servo | ✅ | ✅ | ✅ | `taktwerk-core` (rein) |
| Sockets / Multicast / IGMP | ✅ | ✅ | ✅ | `taktwerk-net` *(geplant)* |
| Virtuelle Soundkarte | PipeWire / snd-aloop | BlackHole (Core Audio) | — *(später)* | `taktwerk-audio` (Trait) |
| mDNS-Discovery | Avahi / pure-Rust | Bonjour nativ | pure-Rust | `taktwerk-discovery` *(geplant)* |
| PTP-Timestamping | SW + `SO_TIMESTAMPING` | nur SW | nur SW | `taktwerk-ptp` (Trait) |
| Router (NMOS/SAP), Web-UI | ✅ | ✅ | ✅ | `taktwerk-router` / UI *(geplant)* |

Die einzige harte OS-Divergenz ist die **virtuelle Soundkarte** — genau der
Teil, der als Compose-Baustein (BlackHole u. a.) eingebunden, nicht selbst
gebaut wird. Deshalb ist Phase 0 **headless**: die komplette Netz-/Protokoll-
Kette läuft ohne Treiber auf jeder Plattform.

---

## Workspace-Layout

```
taktwerk-core       reiner AES67/ST2110-Protokoll- + DSP-Kern (0 OS-Deps)
  ├─ rtp            RTP-Header + L24/L16 Pack/Depack
  ├─ sdp            SDP Build/Parse (Level A) inkl. RFC-7273-Clock-Referenz
  ├─ sap            SAP-Announce/-Parse (RFC 2974)
  ├─ ptp            IEEE-1588-Datentypen + BMCA (Best Master Clock)
  ├─ dsp            ASRC/Clock-Recovery-Servo (PI-Regler)
  └─ clock          TimeSource-Naht (Media-Clock/RTP-Timestamps)
taktwerk-audio      OS-Naht: AudioBackend-Trait + NullBackend (headless)
                    + per-OS-Backends (Feature-gated, ab Phase 1)
taktwerk-net        Multicast-UDP-Sockets + RtpSender/RtpReceiver + SAP-Discovery
                    (tokio + socket2); Beispiele: multicast_selftest, sap_selftest
taktwerk-endpoint   Media-Loop: TxStream (Capture→RTP) + RxStream (RTP→Playback)
taktwerk-router     NMOS IS-04 Node-API + IS-05 Connection-API (Axum)
taktwerk-daemon     Bin `taktwerkd`: REST-API + Web-UI + SAP-Discovery + TX/RX +
                    NMOS-Server; core::ptp: Wire-Format + Servo + PtpTimeSource

Interop verifiziert: RTP L24 gegen GStreamer (beide Richtungen), PTP gegen
linuxptp `ptp4l`. Geplant: React-Ausbau der UI · Audiogeräte-Backends (Phase 1).
```

## Web-UI

Der Daemon liefert unter `http://<TAKTWERK_HTTP>/` eine Bedien-Oberfläche
(Knoten-Status, TX/RX-Steuerung mit Live-Zählern, SAP-Discovery, **Geräte-Übersicht
und Netzwerk-Traffic**). Die NMOS-APIs liegen auf `TAKTWERK_NMOS`
(Default `127.0.0.1:7789`) unter `/x-nmos/`.

**Geräte & Traffic** (`GET /devices`, `GET /traffic`): pro Absender-IP ein Gerät
mit bestem bekannten Namen (SAP-Session / PTP-Clock-ID) und Traffic je Protokoll
(Pakete, Bytes, pps/bps). **Kein Sniffer** — gezählt wird nur der SAP-/PTP-
Control-Traffic und RTP der abonnierten Streams, die der Knoten ohnehin sieht.

## Debug-Log

Der Daemon loggt strukturiert nach **stderr und Datei** (`tracing`):

```bash
TAKTWERK_LOG=debug TAKTWERK_LOG_FILE=taktwerk.log cargo run -p taktwerk-daemon
TAKTWERK_LOG="info,taktwerk_net=trace" cargo run -p taktwerk-daemon   # + per-Paket-RTP
```

Default: eigene Crates `debug`, Fremd-Crates `info`. Per-Paket-Details liegen auf
`trace`. Die Datei wird beim **graceful Shutdown (Ctrl-C)** vollständig geflusht.

## Node starten (headless, Phase 0)

```bash
cargo run -p taktwerk-daemon            # startet `taktwerkd`, REST auf 127.0.0.1:7788
# Konfiguration über Env:
#   TAKTWERK_NAME=mynode TAKTWERK_IFACE=192.168.1.10 TAKTWERK_HTTP=127.0.0.1:7788 TAKTWERK_CH=2

curl localhost:7788/health
curl localhost:7788/node
curl -X POST localhost:7788/streams/tx/start -H 'content-type: application/json' -d '{"channels":2}'
curl localhost:7788/streams/tx           # {"running":true,"packets_sent":...}
curl localhost:7788/streams/discovered   # per SAP entdeckte Streams

# Einen Stream empfangen (abonnieren):
curl -X POST localhost:7788/streams/rx/subscribe -H 'content-type: application/json' -d '{"group":"239.69.83.67","port":5004,"channels":2}'
curl localhost:7788/streams/rx           # {"running":true,"packets_recv":...}
curl -X POST localhost:7788/streams/rx/unsubscribe
curl -X POST localhost:7788/streams/tx/stop
```

---

## Bauen & Testen

```bash
cargo test --workspace          # Kern + Audio, alle Unit-Tests
cargo check --workspace --target aarch64-apple-darwin      # macOS-Portabilität
cargo check --workspace --target x86_64-unknown-linux-gnu  # Linux-Portabilität
```

Phase 0 braucht keinen Mac: Der Kern ist auf Linux entwickel- und gegen
PipeWire-AES67 validierbar (Projektbrief §11, Phase 0).

### Lokaler Hinweis Windows-Dev-Rechner

Auf diesem Rechner sind die MSVC-Build-Tools (Linker) nicht installiert, wohl
aber die **GNU-Toolchain** (mingw). Deshalb hier zum Testen die GNU-Toolchain
verwenden:

```powershell
cargo +stable-x86_64-pc-windows-gnu test --workspace
```

Cross-`check` gegen Linux/macOS läuft über die msvc-Toolchain (linkt nicht):

```powershell
cargo +stable-x86_64-pc-windows-msvc check --workspace --target aarch64-apple-darwin
```

Ernsthafte Builds/Integration (Phase 0-Sandbox, Netz-Tests) laufen ohnehin auf
dem Linux-Server.

---

## Roadmap (Kurzfassung, Details im Projektbrief §11)

- **Phase 0** — Engine-Sandbox (Linux): RTP/SDP/SAP/PTP-Kern, gegen
  PipeWire-AES67 validiert. *(läuft — Kern steht, headless)*
- **Phase 1** — macOS-Endpunkt: BlackHole + Engine + PTP-Slave + ASRC
  (Level-A-Profil: 48 kHz / L24 / 1 ms / ≤8 ch). Parallel Linux-PipeWire-Backend.
- **Phase 2** — Router-Modul: NMOS-Registry/Controller + SAP-Bridge + Grid-UI.
- **Phase 3** — PTP-Master-Toggle: Zustandsmaschine, `priority1`, UI-Warnung.
- **Phase 4** — ST2110-Video-Steuerung (Control-Plane bleibt, Medien nur über Switch).

## Lizenz

MIT OR Apache-2.0 (permissiv, passend zur „kein Dante, keine Lizenz"-Prämisse).
