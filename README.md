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
taktwerk-daemon     Bin `taktwerkd`: REST-API (Axum) + SAP-Discovery + TX-Streaming

geplant: taktwerk-router (NMOS IS-04/05) · taktwerk-ptp (PTP-Wire) · Web-UI (React)
```

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
