# Taktwerk

[![CI](https://github.com/ruehljens-creator/taktwerk/actions/workflows/ci.yml/badge.svg)](https://github.com/ruehljens-creator/taktwerk/actions/workflows/ci.yml)

[🇩🇪 Deutsch](README.md) · **🇬🇧 English**

> **Open, Dante-free AES67 / ST 2110-30 / RAVENNA audio-over-IP node in Rust** —
> a virtual sound card with built-in **NMOS** control plane (IS-04/IS-05), **PTP**
> (IEEE 1588) grandmaster/slave, **SAP + RAVENNA (mDNS/RTSP)** discovery, and a
> web UI showing devices and network traffic. MIT-licensed, cross-platform
> (Linux · macOS · Windows). No Dante, no per-seat licensing.

An open **AES67 node** — a virtual sound card (audio over IP) built on open
standards (AES67 / RAVENNA / ST 2110-30), Dante-free and licence-free. Two
optional roles on top of the endpoint core: **router/crosspoint** (control
plane) and **PTP master clock**. Operated through a shared web frontend.

Full specification (German): [`Taktwerk_Projektbrief.md`](Taktwerk_Projektbrief.md).

**Docs:** Clocking & GPS time (DE/EN) → [`docs/clocking.md`](docs/clocking.md).

*Keywords: AES67, ST 2110, ST 2110-30, RAVENNA, NMOS, IS-04, IS-05, AMWA, PTP,
IEEE 1588, audio over IP, AoIP, RTP L24, SAP, mDNS/DNS-SD, RTSP, SDP, broadcast
audio, media networking, Dante alternative, virtual sound card, Rust.*

> **Status:** Phase 0 — platform-neutral protocol/DSP core. Headless (no virtual
> device), builds and tests on every target OS.

---

## Cross-platform from day one

Target OS (product): **Linux + macOS** first-class; **Windows** compiles the
portable core + router (virtual device later). The guiding principle:

> **The entire protocol/DSP core (`taktwerk-core`) is OS-neutral and depends on
> no platform API.** Everything OS-specific sits behind traits in separate
> crates and is pulled in per `cfg`/feature only on the relevant platform. A
> build on one OS never drags in the backends of the others.

### Portability matrix

| Building block | Linux | macOS (arm64) | Windows | Where |
|---|:---:|:---:|:---:|---|
| RTP L24/L16, SDP, SAP, PTP BMCA, ASRC servo | ✅ | ✅ | ✅ | `taktwerk-core` (pure) |
| Sockets / multicast / IGMP | ✅ | ✅ | ✅ | `taktwerk-net` |
| Virtual sound card | ✅ ALSA `snd-aloop` | Pro Tools Bridge / BlackHole | — *(later)* | `taktwerk-audio` (cpal) |
| mDNS discovery | Avahi / pure-Rust | Bonjour native | pure-Rust | `taktwerk-discovery` |
| PTP timestamping | SW + `SO_TIMESTAMPING` | SW only | SW only | `taktwerk-ptp` (trait) |
| Router (NMOS/SAP), web UI | ✅ | ✅ | ✅ | `taktwerk-router` / UI |

The only hard OS divergence is the **virtual sound card** — precisely the part
that is composed in (BlackHole and friends) rather than built here. That is why
Phase 0 is **headless**: the complete network/protocol chain runs without a
driver on every platform.

---

## Workspace layout

```
taktwerk-core       pure AES67/ST 2110 protocol + DSP core (0 OS deps)
  ├─ rtp            RTP header + L24/L16 pack/depack
  ├─ sdp            SDP build/parse (Level A) incl. RFC 7273 clock reference
  ├─ sap            SAP announce/parse (RFC 2974)
  ├─ ptp            IEEE 1588 data types + BMCA (Best Master Clock)
  ├─ dsp            ASRC / clock-recovery servo (PI controller)
  └─ clock          TimeSource seam (media clock / RTP timestamps)
taktwerk-audio      OS seam: AudioBackend trait + NullBackend (headless)
                    + per-OS backends (feature-gated, from Phase 1)
taktwerk-net        multicast UDP sockets + RtpSender/RtpReceiver + SAP discovery
                    (tokio + socket2); examples: multicast_selftest, sap_selftest
taktwerk-endpoint   media loop: TxStream (capture→RTP) + RxStream (RTP→playback)
taktwerk-router     NMOS IS-04 Node API + IS-05 Connection API (Axum)
taktwerk-discovery  RAVENNA: mDNS/DNS-SD (browse+register) + RTSP (DESCRIBE)
taktwerk-daemon     bin `taktwerkd`: REST API + web UI + SAP/RAVENNA discovery +
                    TX/RX + NMOS + RTSP server + PTP slave/master; core::ptp:
                    wire + BMCA + servo + slave + PtpTimeSource
```

Interop verified: RTP L24 ↔ GStreamer (both directions), PTP slave locks to a
linuxptp `ptp4l` grandmaster, RAVENNA mDNS+RTSP discovery. RAVENNA-compatible
(media/timing/discovery). Planned: React UI · audio-device backends (Phase 1).

## RAVENNA

Taktwerk supports RAVENNA explicitly: a shared media/timing base (RTP L24, PTP,
SDP/RFC 7273) plus RAVENNA's discovery path — **mDNS/DNS-SD** (find sessions and
advertise your own stream, `_ravenna_session._sub._rtsp._tcp`) and **RTSP
`DESCRIBE`** (fetch/serve SDP). With `TAKTWERK_PTP_SLAVE=1` the node locks to the
PTP grandmaster and aligns its media clock accordingly (`GET /ptp`).

## Web UI

The daemon serves an operator interface at `http://<TAKTWERK_HTTP>/` (node
status, TX/RX control with live counters, SAP discovery, **device overview and
network traffic**, plus a **clock panel** with drift and GNSS satellite status).
The NMOS APIs live on `TAKTWERK_NMOS` (default `127.0.0.1:7789`) under `/x-nmos/`.

**Devices & traffic** (`GET /devices`, `GET /traffic`): one device per source IP
with the best known name (SAP session / PTP clock ID) and per-protocol traffic
(packets, bytes, pps/bps). **Not a sniffer** — it counts only the SAP/PTP control
traffic and the RTP of subscribed streams that the node sees anyway.

## Clocking & GPS

A dedicated guide covers how a node can be clocked and how to feed a precise
GPS/GNSS reference into a computer (USB GNSS puck with PPS, Intel NIC + GPS
module on an SDP pin, PCIe TimeCard, …): [`docs/clocking.md`](docs/clocking.md).

The PTP master supports the **ST 2059-2 broadcast profile**
(`TAKTWERK_PTP_PROFILE=st2059` → domain 127, Sync 8/s, Announce 4/s) and
individual tunables (`TAKTWERK_PTP_DOMAIN`, `…_PRIORITY1/2`, `…_CLOCK_CLASS`,
`…_SYNC_MS`, `…_ANNOUNCE_MS`).

## Debug log

The daemon logs structured output to **stderr and file** (`tracing`):

```bash
TAKTWERK_LOG=debug TAKTWERK_LOG_FILE=taktwerk.log cargo run -p taktwerk-daemon
TAKTWERK_LOG="info,taktwerk_net=trace" cargo run -p taktwerk-daemon   # + per-packet RTP
```

Default: own crates `debug`, foreign crates `info`. Per-packet detail is at
`trace`. The file is fully flushed on **graceful shutdown (Ctrl-C)**.

## Real audio device (Phase 1, optional)

By default the node is **headless** (no audio device, `NullBackend`). With the
`cpal` feature it uses a real input/output device (WASAPI · CoreAudio · ALSA):

```bash
cargo run -p taktwerk-daemon --features cpal   # then enable via env:
TAKTWERK_AUDIO=cpal cargo run -p taktwerk-daemon --features cpal
# list devices:
cargo run -p taktwerk-audio --features cpal-backend --example audio_devices
```

TX then captures from the default input device, RX plays to the default output
device. Select a **specific device by name** via `TAKTWERK_AUDIO_IN` (capture)
and `TAKTWERK_AUDIO_OUT` (playback) — exact or substring match, e.g. the Pro
Tools Bridge as an AES67↔DAW device:

```bash
TAKTWERK_AUDIO=cpal TAKTWERK_AUDIO_IN="Pro Tools Audio Bridge 2" \
  cargo run -p taktwerk-daemon --features cpal
# device choice is testable in the example too (arg1=capture, arg2=playback):
cargo run -p taktwerk-audio --features cpal-backend --example audio_devices -- \
  "Pro Tools Audio Bridge 2" "Pro Tools Audio Bridge 2"
```

*(Linux needs `libasound2-dev` + `pkg-config` to build the feature.)*

## Virtual sound card

So that a DAW/app sees the node as a **system audio device** (what is played in →
AES67, received AES67 → recordable), Taktwerk uses a virtual device and opens it
via cpal device selection by name:

- **Linux (headless):** ALSA kernel loopback **`snd-aloop`** — no PipeWire/
  PulseAudio needed. Setup: `sudo deploy/linux-snd-aloop.sh` (loads the module
  persistently + installs `libasound2-dev`/`pkg-config`). Then:
  ```bash
  TAKTWERK_AUDIO=cpal TAKTWERK_AUDIO_IN=Loopback  taktwerkd   # app → AES67 (TX)
  TAKTWERK_AUDIO=cpal TAKTWERK_AUDIO_OUT=Loopback taktwerkd   # AES67 → app (RX)
  ```
  Self-test (audio flows through the virtual device into the capture path):
  ```bash
  cargo run -p taktwerk-audio --features cpal-backend --example loopback_flow \
    -- Loopback,DEV=1 Loopback,DEV=0
  ```
- **macOS:** pick an existing virtual device by name — e.g. **"Pro Tools Audio
  Bridge"** or BlackHole (`TAKTWERK_AUDIO_IN="Pro Tools Audio Bridge 64"`).

Both ends run AES67-compliant at **48 kHz** (snd-aloop requires the same
rate/channel count at both ends of the cable).

## Running a node (headless, Phase 0)

```bash
cargo run -p taktwerk-daemon            # starts `taktwerkd`, REST on 127.0.0.1:7788
# configuration via env:
#   TAKTWERK_NAME=mynode TAKTWERK_IFACE=192.168.1.10 TAKTWERK_HTTP=127.0.0.1:7788 TAKTWERK_CH=2

curl localhost:7788/health
curl localhost:7788/node
curl -X POST localhost:7788/streams/tx/start -H 'content-type: application/json' -d '{"channels":2}'
# high channel counts (up to 64): packet time is chosen MTU-safe automatically
#   ≤8→1ms · ≤16→500µs · ≤32→250µs · ≤64→125µs (1152 B payload each, no jumbo)
curl -X POST localhost:7788/streams/tx/start -H 'content-type: application/json' -d '{"channels":64}'
curl localhost:7788/streams/tx           # list of all TX streams [{id,dest,packets_sent},…]
curl localhost:7788/streams/discovered   # streams discovered via SAP

# multiple streams at once (multi-stream): just call start again with a
# different group/port. Key per stream = "group:port".
curl -X POST localhost:7788/streams/rx/subscribe -H 'content-type: application/json' -d '{"group":"239.69.83.67","port":5004,"channels":2}'
curl localhost:7788/streams/rx           # list of all RX subscriptions [{id,source,packets_recv},…]
# stop one specific stream (without ?id= → all):
curl -X POST 'localhost:7788/streams/rx/unsubscribe?id=239.69.83.67:5004'
curl -X POST 'localhost:7788/streams/tx/stop?id=239.69.83.67:5004'
```

---

## Configuration & autostart

All settings go through `TAKTWERK_*` environment variables **or** an optional
TOML file (`TAKTWERK_CONFIG`, otherwise `./taktwerk.toml` or
`/etc/taktwerk/taktwerk.toml`). **Env takes precedence** over the file. Template:
[`deploy/taktwerk.example.toml`](deploy/taktwerk.example.toml).

As a service (continuous operation, clean log flush on stop):

- **Linux (systemd):** [`deploy/taktwerkd.service`](deploy/taktwerkd.service) —
  `systemctl enable --now taktwerkd` (installation steps at the top of the file).
- **macOS (launchd):** [`deploy/com.taktwerk.daemon.plist`](deploy/com.taktwerk.daemon.plist)
  in `/Library/LaunchDaemons/` (runs without login).
- **Linux virtual sound card** set up beforehand: [`deploy/linux-snd-aloop.sh`](deploy/linux-snd-aloop.sh).

## Build & test

```bash
cargo test --workspace          # core + audio, all unit tests
cargo check --workspace --target aarch64-apple-darwin      # macOS portability
cargo check --workspace --target x86_64-unknown-linux-gnu  # Linux portability
```

Phase 0 needs no Mac: the core is developed on Linux and validated against
PipeWire-AES67 (project brief §11, Phase 0).

### Local note for the Windows dev machine

On that machine the MSVC build tools (linker) are not installed, but the **GNU
toolchain** (mingw) is. So use the GNU toolchain there for testing:

```powershell
cargo +stable-x86_64-pc-windows-gnu test --workspace
```

Cross-`check` against Linux/macOS runs via the msvc toolchain (does not link):

```powershell
cargo +stable-x86_64-pc-windows-msvc check --workspace --target aarch64-apple-darwin
```

Serious builds/integration (Phase 0 sandbox, network tests) run on the Linux
server anyway.

---

## Roadmap (short form, details in project brief §11)

- **Phase 0** — engine sandbox (Linux): RTP/SDP/SAP/PTP core, validated against
  PipeWire-AES67. *(running — core is in place, headless)*
- **Phase 1** — macOS endpoint: BlackHole + engine + PTP slave + ASRC
  (Level A profile: 48 kHz / L24 / 1 ms / ≤8 ch). Linux PipeWire backend in parallel.
- **Phase 2** — router module: NMOS registry/controller + SAP bridge + grid UI.
- **Phase 3** — PTP master toggle: state machine, `priority1`, UI warning.
- **Phase 4** — ST 2110 video control (control plane stays, media only through the switch).

## License

**MIT** (see [LICENSE](LICENSE)) — permissive, matching the "no Dante, no
licence" premise. Dependencies are exclusively MIT or MIT/Apache-2.0 licensed.
