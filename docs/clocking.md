# Taktung & GPS-Zeit / Clocking & GPS Time

> Wie sich ein Taktwerk-Knoten takten lässt — und wie man einen **präzisen
> GPS-/GNSS-Takt** in einen Computer bekommt (USB-Maus mit PPS, Intel-NIC +
> GPS-Modul, PCIe-TimeCard …).
>
> How a Taktwerk node can be clocked — and how to get a **precise GPS/GNSS
> reference** into a computer (USB puck with PPS, Intel NIC + GPS module,
> PCIe TimeCard …).

🇩🇪 [Deutsch](#deutsch) · 🇬🇧 [English](#english)

---

<a id="deutsch"></a>

## 🇩🇪 Deutsch

### 1. Warum überhaupt takten?

Audio-over-IP (AES67 / ST 2110-30) verlangt, dass **alle Geräte am selben Takt
hängen**. Sender und Empfänger tasten mit exakt denselben 48 000 Samples/s ab.
Driften zwei Uhren auch nur um wenige ppm auseinander, laufen die Puffer über
oder leer — hörbar als Knackser, Aussetzer oder langsames „Wandern“.

In einem AES67-Netz übernimmt **PTP (IEEE 1588 / Precision Time Protocol)** die
gemeinsame Zeitbasis. Genau **eine** Uhr im Netz ist **Grandmaster**, alle
anderen takten sich als **Slave** darauf auf. Welche Uhr Grandmaster wird,
entscheidet die BMCA (Best Master Clock Algorithm) automatisch anhand von
Qualitäts-Kennzahlen (`priority1`, `clockClass`, `clockAccuracy`, …).

Der entscheidende Punkt: **PTP verteilt die Zeit, erzeugt sie aber nicht.** Die
Qualität des Grandmasters hängt daran, woran *er selbst* hängt. Ein Grandmaster,
der frei läuft, ist besser als nichts — aber ein Grandmaster, der an **GPS**
diszipliniert ist, gibt dem ganzen Netz eine absolute, langzeitstabile Zeit.

### 2. Taktquellen, die Taktwerk kennt

Taktwerk kann heute jede dieser Rollen einnehmen. Die aktive Quelle wählt man
im Web-UI (Clock-Panel → Quelle) oder per `POST /clock/source`; im Code stehen
die IDs in `CLOCK_SOURCES`:

| ID       | Quelle                          | Status im Code | Beschreibung |
|----------|---------------------------------|----------------|--------------|
| `auto`   | Automatik (BMCA)                | ✅ aktiv        | Beste verfügbare Quelle gewinnt automatisch. |
| `ptp`    | Externer PTP-Grandmaster        | ✅ aktiv        | Taktwerk läuft als **Slave**, lockt auf einen fremden GM (z. B. eine echte Broadcast-Masterclock). |
| `gnss`   | GPS/GNSS-Zeit                   | ✅ Anzeige, ⏳ Disziplinierung | GNSS-Panel zeigt Fix/SNR (via `gpsd`); die *Disziplinierung* der PTP-Quelle folgt (siehe §4). |
| `system` | System-/RTC-Uhr                 | ✅ aktiv        | Freilauf auf der lokalen Rechneruhr — Fallback ohne Referenz. |
| `aes`    | Haustakt über Audio-Eingang    | ⏳ Hardware      | Externer Wortakt liegt als Signal an einem AES-Eingang an (z. B. Rosendahl nanosync). |
| `wcpps`  | Wordclock → 1-Hz-Teiler         | ⏳ Hardware      | Wordclock/Blackburst wird auf einen PPS heruntergeteilt und disziplinert die Uhr. |

Als **Master** stellt Taktwerk selbst einen PTP-Grandmaster bereit
(`TAKTWERK_PTP_MASTER=1`), wahlweise im **ST-2059-2-Broadcast-Profil**
(`TAKTWERK_PTP_PROFILE=st2059` → Domain 127, Sync 8/s, Announce 4/s). Sobald der
Master an einer disziplinierten Referenz hängt, setzt man `clockClass = 6`
(„von primärer Referenz — GPS — synchronisiert“); ohne Referenz gilt der
Default 248 („freilaufend“).

### 3. Der Weg der Zeit in Taktwerk

```
  GPS-/GNSS-Satelliten
         │  (NMEA-Daten + 1 PPS-Impuls pro Sekunde)
         ▼
  GNSS-Empfänger  ──►  gpsd / chrony  ──►  PHC (PTP Hardware Clock)
   (USB / NIC)          (Kernel-Zeit)        auf der Netzwerkkarte
                                                │
                                                ▼
                                     Taktwerk PTP-Master  ──►  AES67-Netz
                                     (clockClass 6)             (alle Slaves)
```

Kurz: Der **PPS-Impuls** (Puls pro Sekunde) ist der eigentliche Präzisionsträger.
Die NMEA-Daten über USB/Seriell sagen nur *welche* Sekunde es ist (auf ~100 ms
genau); die **steigende Flanke des PPS** markiert deren Beginn auf **einige zehn
Nanosekunden** genau. Ziel ist immer, diesen PPS an einen möglichst gut
timestampenden Punkt zu bringen — idealerweise direkt an die **PHC der NIC**.

### 4. GPS-Zeit in den Computer bekommen — die Wege

Von „billig & einfach“ bis „broadcast-präzise“:

#### 4a. USB-GNSS-Maus mit PPS *(einfachster Einstieg)*

Eine GNSS-„Maus“ (G-Mouse, u‑blox-Chipsatz) am USB-Port. Wichtig ist ein Modul,
das **PPS herausführt** — viele billige tun das *nicht*.

- **NMEA** kommt über den USB-CDC-Seriellport (`/dev/ttyACM0` o. ä.).
- **PPS**: Bei USB-Mäusen mit PPS-Leitung wird der Impuls je nach Modell über
  ein Steuersignal des Seriellports gemeldet (DCD/CTS). Sauberer ist ein Modul,
  dessen PPS-Pin man **direkt** auf einen echten Seriell-Header (COM-Port,
  DCD-Pin) oder einen GPIO legt.
- **Software**: `gpsd` liest NMEA + PPS, `chrony` diszipliniert daraus die
  Kernel-Systemzeit, `ptp4l`/`phc2sys` überträgt sie auf die NIC-PHC.
- **Genauigkeit**: gut für Zeit auf ~1–50 µs; für Audio-Sample-Sync völlig
  ausreichend, weil die *Frequenz*stabilität zählt und PPS langzeitgenau ist.

> **Ist bereits bestellt** (DIYmalls G‑Mouse mit PPS-Zugriff). Sobald sie da
> ist: USB einstecken, PPS auf den COM-Header (DCD), dann `gpsd` + Kernel-PPS
> (`ldattach`/`ppstest`) + `chrony` (NMEA + PPS). Danach lebt das GNSS-Panel und
> die Quelle `gnss` wird echt wirksam.

#### 4b. Serieller/GPIO-PPS *(sauberer als USB)*

USB fügt Jitter hinzu (Polling, Sammelübertragung). Deutlich besser ist der PPS
über eine **echte serielle Schnittstelle** (DCD-Pin des COM-Ports) oder einen
**GPIO** (z. B. Raspberry Pi). Der Kernel stempelt die Flanke per Interrupt —
Jitter im **einstelligen Mikrosekunden**-Bereich. Modul-NMEA kann weiterhin
über USB/UART laufen; nur der PPS braucht den präzisen Pfad.

#### 4c. Intel-NIC + GPS-Modul auf den SDP-Pin *(der „Sweet Spot“)*

**Das ist der interessante Weg für Audio-over-IP.** Bestimmte Intel-Netzwerk‑
karten haben eine eigene **PTP Hardware Clock (PHC)** *und* nach außen geführte
**SDP-Pins** (Software Definable Pins), die einen externen **PPS-Eingang**
timestampen können:

- **Intel i210** (z. B. als preiswerte PCIe-Karte) — SDP0–SDP3, PPS-In/-Out.
- **Intel i225/i226** — ebenfalls mit SDP.
- Auf vielen Karten sind die SDP-Pins auf einem **Stiftheader** oder Lötpad
  herausgeführt.

Ablauf:

```
GPS-Modul ── PPS ──►  SDP0 der i210        (Hardware-Timestamp direkt auf der NIC)
GPS-Modul ── NMEA ─►  USB/UART ──► gpsd    (nur "welche Sekunde")
                                    │
   ts2phc  ◄───────────────────────┘   diszipliniert die NIC-PHC auf den PPS
   phc2sys                             (verteilt PHC → System bzw. andere NICs)
   ptp4l   ── Taktwerk-Master ──► AES67-Netz  (clockClass 6, GPS-synchron)
```

Der Clou: Der PPS wird **direkt auf der Karte** gestempelt, die auch die
PTP-Pakete stempelt — kein USB-, kein Kernel-Scheduling-Jitter dazwischen. Mit
`linuxptp`-`ts2phc` diszipliniert man die PHC auf den externen PPS, `ptp4l`
verteilt sie ins Netz. Genauigkeit: **Zehner-Nanosekunden**. Damit wird ein
gewöhnlicher Linux-Rechner (z. B. der **Futro**) zu einer ernstzunehmenden,
GPS-disziplinierten AES67-Grandmaster — für einen Bruchteil der Kosten einer
fertigen Broadcast-Masterclock.

**Verdrahtungs-Beispiel (i210 + u‑blox-Modul):**

| GPS-Modul | → | Intel i210 | Zweck |
|-----------|---|-----------|-------|
| `PPS`     | → | `SDP0` (Header/Pad) | 1-Hz-Impuls, HW-Timestamp |
| `TX` (NMEA) | → | USB/UART am Rechner | „welche Sekunde“ (gpsd) |
| `GND`     | → | `GND` | gemeinsame Masse (Pflicht!) |
| `VCC`     | ← | 3V3/5V | Modulversorgung |

> ⚠️ **Massepflicht**: Ohne gemeinsame Masse zwischen Modul und NIC ist der
> PPS-Flankenzeitpunkt unbrauchbar. Kurze, gleich lange Leitungen bevorzugen.

#### 4d. PCIe-TimeCard / OCP TAP *(High-End)*

Die **OCP Time Appliance Project TimeCard** (und kommerzielle Derivate) ist eine
PCIe-Karte mit GNSS-Empfänger **und** einem gehaltenen Oszillator
(OCXO/Rubidium) an Bord. Sie hält die Zeit auch bei GPS-Ausfall über Stunden im
Nanosekundenbereich („Holdover“) und stellt sich dem System als hochgenaue PHC
zur Verfügung. Deutlich teurer, aber die Referenz für ernsthafte
GPS-Grandmaster ohne externe Hardware.

#### 4e. Fertige Broadcast-Masterclock *(kaufen statt bauen)*

Geräte wie **Rosendahl nanosync**, **Meinberg**, **Evertz 5700MSC** liefern
PTP-Grandmaster + Wordclock + Blackburst + GPS-Disziplinierung schlüsselfertig.
Taktwerk kann sich als **Slave** (`source = ptp`) darauf aufsynchronisieren —
das ist der Regelfall in einem bestehenden Studio.

### 5. Und Blackburst / Wordclock / AES-Haustakt?

Wenn schon ein **Studio-Haustakt** existiert (Wordclock, Blackburst/Tri-Level,
oder Takt an einem AES-Eingang), ist *der* die natürliche Referenz — dann muss
kein GPS her. Zwei Wege:

- **Frequenz-Referenz**: Der Haustakt gibt nur die *Frequenz* (kein absolute
  Zeit). Für reines Audio-Sample-Sync reicht das. Wordclock (48 kHz) lässt sich
  auf 1 PPS herunterteilen (`wcpps`) und diszipliniert damit die Uhr.
- **Kombiniert**: Haustakt für die Frequenz + GPS für die *absolute Zeit* ist
  die Broadcast-Ideallösung (genau das machen die Geräte aus §4e intern).

Blackburst hilft **nicht direkt** als Zeitquelle (es trägt keine Uhrzeit), aber
sehr wohl als **Frequenzreferenz** — genau wie Wordclock.

### 6. Zusammenfassung / Empfehlung

| Ziel | Empfohlener Weg |
|------|-----------------|
| Schnell testen | USB-GNSS-Maus mit PPS (§4a) |
| Guter, günstiger GPS-Grandmaster | Intel i210 + GPS-Modul auf SDP0 (§4c) |
| Höchste Präzision, Holdover | PCIe-TimeCard / OCP TAP (§4d) |
| In bestehendem Studio | Als PTP-Slave auf vorhandene Masterclock (§4e) |
| Nur Frequenz-Sync nötig | Vorhandener Haustakt / Wordclock (§5) |

---

<a id="english"></a>

## 🇬🇧 English

### 1. Why clock at all?

Audio-over-IP (AES67 / ST 2110-30) requires that **every device runs off the
same clock**. Senders and receivers must sample at exactly the same 48 000
samples/s. If two clocks drift apart by even a few ppm, buffers overrun or
starve — audible as clicks, dropouts or slow "wander".

In an AES67 network, **PTP (IEEE 1588 / Precision Time Protocol)** provides the
shared time base. Exactly **one** clock is the **grandmaster**; every other node
locks to it as a **slave**. Which clock becomes grandmaster is decided
automatically by the BMCA (Best Master Clock Algorithm) using quality metrics
(`priority1`, `clockClass`, `clockAccuracy`, …).

The key insight: **PTP distributes time, it doesn't create it.** A grandmaster's
quality depends on what *it* is locked to. A free-running grandmaster is better
than nothing — but a grandmaster disciplined to **GPS** gives the whole network
an absolute, long-term-stable time reference.

### 2. Clock sources Taktwerk knows about

Taktwerk can take on any of these roles today. Select the active source in the
web UI (Clock panel → Source) or via `POST /clock/source`; the IDs live in
`CLOCK_SOURCES` in the code:

| ID       | Source                          | Code status | Description |
|----------|---------------------------------|-------------|-------------|
| `auto`   | Automatic (BMCA)                | ✅ active    | Best available source wins automatically. |
| `ptp`    | External PTP grandmaster        | ✅ active    | Taktwerk runs as **slave**, locks to a foreign GM (e.g. a real broadcast master clock). |
| `gnss`   | GPS/GNSS time                   | ✅ display, ⏳ discipline | GNSS panel shows fix/SNR (via `gpsd`); *disciplining* the PTP source follows (see §4). |
| `system` | System/RTC clock                | ✅ active    | Free-run on the local machine clock — fallback with no reference. |
| `aes`    | House clock via audio input     | ⏳ hardware  | External word clock present on an AES input (e.g. Rosendahl nanosync). |
| `wcpps`  | Word clock → 1 Hz divider       | ⏳ hardware  | Word clock/black burst divided down to a PPS that disciplines the clock. |

As a **master**, Taktwerk provides its own PTP grandmaster
(`TAKTWERK_PTP_MASTER=1`), optionally in the **ST 2059-2 broadcast profile**
(`TAKTWERK_PTP_PROFILE=st2059` → domain 127, Sync 8/s, Announce 4/s). Once the
master is locked to a disciplined reference, set `clockClass = 6`
("synchronised to a primary reference — GPS"); without a reference the default
248 ("free-running") applies.

### 3. The path of time through Taktwerk

```
  GPS/GNSS satellites
         │  (NMEA data + 1 PPS pulse per second)
         ▼
  GNSS receiver  ──►  gpsd / chrony  ──►  PHC (PTP Hardware Clock)
   (USB / NIC)         (kernel time)        on the network card
                                                │
                                                ▼
                                     Taktwerk PTP master  ──►  AES67 network
                                     (clockClass 6)            (all slaves)
```

In short: the **PPS pulse** (pulse per second) carries the actual precision. The
NMEA data over USB/serial only tells you *which* second it is (to ~100 ms); the
**rising edge of the PPS** marks the start of that second to within **a few tens
of nanoseconds**. The goal is always to bring that PPS to the best-timestamping
point available — ideally straight onto the **NIC's PHC**.

### 4. Getting GPS time into the computer — the options

From "cheap & easy" to "broadcast-precise":

#### 4a. USB GNSS puck with PPS *(simplest start)*

A GNSS "puck" (G-Mouse, u‑blox chipset) on a USB port. What matters is a module
that **exposes PPS** — many cheap ones do *not*.

- **NMEA** arrives over the USB CDC serial port (`/dev/ttyACM0` etc.).
- **PPS**: on USB pucks with a PPS line the pulse is reported, depending on the
  model, via a serial control signal (DCD/CTS). Cleaner is a module whose PPS
  pin you route **directly** to a real serial header (COM port, DCD pin) or a
  GPIO.
- **Software**: `gpsd` reads NMEA + PPS, `chrony` disciplines the kernel system
  clock from it, `ptp4l`/`phc2sys` carries it onto the NIC PHC.
- **Accuracy**: good for time to ~1–50 µs; more than enough for audio sample
  sync, because what matters is *frequency* stability and PPS is long-term
  accurate.

> **Already ordered** (DIYmalls G‑Mouse with PPS access). Once it arrives: plug
> in USB, PPS onto the COM header (DCD), then `gpsd` + kernel PPS
> (`ldattach`/`ppstest`) + `chrony` (NMEA + PPS). After that the GNSS panel
> comes alive and the `gnss` source becomes truly effective.

#### 4b. Serial/GPIO PPS *(cleaner than USB)*

USB adds jitter (polling, batched transfer). Much better is PPS over a **real
serial port** (the COM port's DCD pin) or a **GPIO** (e.g. Raspberry Pi). The
kernel timestamps the edge on an interrupt — jitter in the **single-digit
microseconds**. Module NMEA can still run over USB/UART; only the PPS needs the
precise path.

#### 4c. Intel NIC + GPS module onto the SDP pin *(the sweet spot)*

**This is the interesting path for audio-over-IP.** Certain Intel network cards
have their own **PTP Hardware Clock (PHC)** *and* exposed **SDP pins** (Software
Definable Pins) that can timestamp an external **PPS input**:

- **Intel i210** (e.g. as a cheap PCIe card) — SDP0–SDP3, PPS in/out.
- **Intel i225/i226** — also with SDP.
- On many cards the SDP pins are brought out to a **pin header** or solder pad.

Flow:

```
GPS module ── PPS ──►  SDP0 of the i210      (hardware timestamp right on the NIC)
GPS module ── NMEA ─►  USB/UART ──► gpsd     (only "which second")
                                    │
   ts2phc  ◄───────────────────────┘   disciplines the NIC PHC to the PPS
   phc2sys                             (distributes PHC → system / other NICs)
   ptp4l   ── Taktwerk master ──► AES67 network  (clockClass 6, GPS-locked)
```

The trick: the PPS is timestamped **on the very card** that also timestamps the
PTP packets — no USB or kernel-scheduling jitter in between. With
`linuxptp`'s `ts2phc` you discipline the PHC to the external PPS, `ptp4l`
distributes it to the network. Accuracy: **tens of nanoseconds**. This turns an
ordinary Linux box (e.g. the **Futro**) into a serious GPS-disciplined AES67
grandmaster — at a fraction of the cost of an off-the-shelf broadcast master
clock.

**Wiring example (i210 + u‑blox module):**

| GPS module | → | Intel i210 | Purpose |
|------------|---|-----------|---------|
| `PPS`      | → | `SDP0` (header/pad) | 1 Hz pulse, HW timestamp |
| `TX` (NMEA) | → | USB/UART on the host | "which second" (gpsd) |
| `GND`      | → | `GND` | common ground (mandatory!) |
| `VCC`      | ← | 3V3/5V | module power |

> ⚠️ **Common ground is mandatory**: without a shared ground between module and
> NIC the PPS edge timing is useless. Prefer short, equal-length leads.

#### 4d. PCIe TimeCard / OCP TAP *(high end)*

The **OCP Time Appliance Project TimeCard** (and commercial derivatives) is a
PCIe card with a GNSS receiver **and** a holdover oscillator (OCXO/rubidium) on
board. It keeps time in the nanosecond range for hours even if GPS drops
("holdover") and presents itself to the system as a highly accurate PHC. Much
more expensive, but the reference for a serious GPS grandmaster with no external
hardware.

#### 4e. Off-the-shelf broadcast master clock *(buy instead of build)*

Devices like **Rosendahl nanosync**, **Meinberg**, **Evertz 5700MSC** provide
PTP grandmaster + word clock + black burst + GPS disciplining turn-key. Taktwerk
can synchronise to them as a **slave** (`source = ptp`) — the normal case in an
existing studio.

### 5. What about black burst / word clock / AES house clock?

If a **studio house clock** already exists (word clock, black burst/tri-level,
or a clock on an AES input), *that* is the natural reference — no GPS needed.
Two paths:

- **Frequency reference**: the house clock only gives you the *frequency* (no
  absolute time). For pure audio sample sync that's enough. Word clock (48 kHz)
  can be divided down to 1 PPS (`wcpps`) to discipline the clock.
- **Combined**: house clock for frequency + GPS for *absolute* time is the ideal
  broadcast solution (exactly what the devices in §4e do internally).

Black burst is **not** directly usable as a time source (it carries no
time-of-day), but it is perfectly good as a **frequency reference** — just like
word clock.

### 6. Summary / recommendation

| Goal | Recommended path |
|------|------------------|
| Quick test | USB GNSS puck with PPS (§4a) |
| Good, cheap GPS grandmaster | Intel i210 + GPS module on SDP0 (§4c) |
| Highest precision, holdover | PCIe TimeCard / OCP TAP (§4d) |
| In an existing studio | As a PTP slave to the existing master clock (§4e) |
| Only frequency sync needed | Existing house clock / word clock (§5) |

---

*Part of [Taktwerk](../README.md) · MIT-licensed · see also the PTP profile
settings in [`deploy/taktwerk.example.toml`](../deploy/taktwerk.example.toml).*
