# Projektbrief — AES67-Node für macOS

**Arbeitstitel:** *Taktwerk* (Platzhalter, passt zum -werk-Muster von Lightwerk/Leitwerk; frei änderbar)
**Kurzbeschreibung:** Ein offenes macOS-Programm, das als AES67-Endpunkt (virtuelle Soundkarte) arbeitet und bei Bedarf zwei zuschaltbare Rollen übernimmt — **Router/Kreuzschiene** (Control-Plane) und **PTP-Masterclock**. Ziel ist eine Dante-freie, lizenzfreie Audio-over-IP-Basis auf Basis offener Standards (AES67/RAVENNA).

---

## 1. Zielsetzung

Ein Gerät (Mac), das

1. **immer** ein AES67-Endpunkt ist — d. h. eine virtuelle Core-Audio-Soundkarte, deren Kanäle als AES67-Streams ins Netz gehen und aus dem Netz kommen (Pro Tools & Co. sehen sie wie eine normale Karte);
2. **optional** als **Router** eine AES67/ST2110-Kreuzschiene bereitstellt (NMOS + SAP/SDP);
3. **optional** als **PTP-Masterclock** die Zeitreferenz für ein kleines Netz liefert.

Bedienung über ein gemeinsames Web-Frontend. Der Kern schließt die offene Lücke, die auf Linux längst gelöst ist (PipeWire-AES67), auf macOS bisher aber nicht offen existiert — relevant, weil die Pro-Tools-Rigs auf macOS laufen und dort kein PipeWire greift.

---

## 2. Architekturprinzip: Control-Plane vs. Media-Plane

Der wichtigste Leitsatz, der das ganze Design bestimmt:

- **Media-Plane** (die eigentlichen Audio-/Video-Multicast-Streams) läuft über den **Switch**, nie durch dieses Gerät hindurch. Das gilt besonders für ST2110-Video (~1,5 Gbit/s HD bis ~12 Gbit/s UHD pro Stream) — ein Mac trägt das nicht und soll es nicht.
- **Control-Plane** (wer abonniert wen, wer ist Master) ist das, was dieses Programm macht: orchestrieren, nicht transportieren.
- **Ausnahme:** Nur der **eigene** Endpunkt-Audiopfad (die virtuelle Soundkarte) verarbeitet echte Samples — und der ist strikt von der Control-Plane isoliert (siehe §4).

---

## 3. Module

### 3.1 Kern (immer aktiv): AES67-Endpunkt / virtuelle Soundkarte
- **Virtuelles Core-Audio-Gerät:** über **BlackHole** (ExistentialAudio, GPL-3.0, AudioServerPlugin, Intel+Apple Silicon, 2/16/64…256+ Kanäle). Nicht selbst nachbauen — als Baustein einbinden.
- **AES67/RTP-Engine** (eigen, User-Space): RTP-Paketierung/Depacketizing, L24/L16, Multicast-Join, SDP-Erzeugung/-Parsing, SAP- **und** mDNS/Bonjour-Discovery.
- **PTP-Client** (Slave): lockt den Audiotakt an den Netz-Grandmaster.
- **Clock-Recovery + ASRC:** überbrückt die BlackHole-Clock-Domäne und den AES67-Netztakt per adaptivem Resampling (DVS-Modell).

### 3.2 Modul „Router" (Toggle): Control-Plane-Kreuzschiene
- **NMOS IS-04** (Registry/Discovery) + **IS-05** (Connection Management, HTTP-PATCH staged→activate) + optional **IS-08** (Audio-Channel-Mapping).
- **SAP/SDP-Bridge** für AES67-Altgeräte ohne NMOS.
- Baustein-Optionen: `nmos-cpp` (Sony, Apache-2.0) als Registry/Controller, `nmos-js` als fertiges React-Grid — oder eigenes Grid über dieselben APIs.
- Berührt den Audiopfad nicht → beliebig zu-/abschaltbar.

### 3.3 Modul „PTP-Master" (Toggle): Zeitreferenz
- Schaltet die PTP-Engine von **Slave** auf **Master** (siehe Zustandsmaschine §5).
- `priority1` bewusst setzen, damit die Box die BMCA-Wahl gewinnt.

### 3.4 Web-Frontend
- Ein UI, drei Panels: **Soundkarten-I/O**, **Router-Grid** (Sender × Receiver), **PTP-Master-Toggle + Status**.
- Backend-Abstraktion als ein `RouterBackend`-Trait, hinter dem NMOS *und* SAP *und* PTP liegen (das UI bleibt backend-agnostisch).

### 3.5 Config-/State-Store
- Lokal ausreichend (SQLite/JSON). Kein PostgreSQL nötig (anders als bei Leitwerk).

---

## 4. Prozess- und Thread-Modell

Strikte Isolation zwischen Real-time und Control:

- **Audio-Engine = eigener, hochpriorisierter Prozess** (Real-time-Thread, ggf. Core-Pinning). Darf **nie** durch Web-Requests, NMOS-Polls oder DB-Zugriffe blockiert werden.
- **Router + Web-UI + State = separate(r) Prozess(e).** Control-Plane darf die Real-time-Plane nicht anfassen.
- Kommunikation zwischen den Prozessen über lokale IPC (Socket/REST), nicht über geteilten Real-time-Speicher.
- **Lizenz-Nebeneffekt:** Weil die Engine ein **separater Prozess** ist, der BlackHole nur über das Core-Audio-Gerät nutzt (statt BlackHole-Code zu linken), bleibt die Engine lizenz-frei wählbar trotz BlackHoles GPL-3.0.

---

## 5. PTP-Zustandsmaschine (der zentrale Toggle-Konflikt)

Ein Gerät kann **nicht gleichzeitig Slave und Master** sein. Der „PTP-Master"-Schalter ist deshalb ein Rollenwechsel, kein additives Feature:

| Zustand | Audiotakt der Soundkarte | Einsatz |
|---|---|---|
| **Master AUS** | lockt via BMCA an **externen** Grandmaster | Normalfall im bestehenden Netz |
| **Master AN** | lockt an den **eigenen** lokalen Takt | wenn diese Box das ganze kleine Rig taktet |

- Beim Umschalten: PTP-Engine sauber neu initialisieren, ASRC-Servo zurücksetzen, kurze Re-Lock-Phase einplanen.
- **BMCA-Warnung:** Existiert ein höher-priorisierter GM, verliert dein „Master" die Wahl. `priority1` muss aktiv gesetzt werden, damit die Box gewinnt — sonst still im Slave.

---

## 6. Clock-Domänen und Datenfluss

```
Pro Tools / DAW
      │  (Core Audio)
 BlackHole (virtuelles Gerät)   ← Clock-Domäne A (Core-Audio-Gerätetakt)
      │
   ASRC / Clock-Recovery         ← die kritische Brücke
      │
 AES67-RTP-Engine               ← Clock-Domäne B (PTP-Netztakt)
      │  (Multicast RTP)
   Netzwerk / Switch
```

Der ASRC-Punkt ist der DSP-Knackpunkt: Er diszipliniert Domäne A auf Domäne B (bzw. umgekehrt im Master-Fall). Genau hier entstehen bei schlechter Auslegung Drift, Klicks und Latenz.

---

## 7. AES67/ST-2110-30-Zielprofil

Der Kern-Trick des Projekts: **ein Codepfad, beide Welten.** ST 2110-30 ist eine eingeschränkte Teilmenge von AES67 — wenn die Engine das gemeinsame Level trifft, sind ihre Streams *gleichzeitig* gültiges AES67 **und** ST-2110-30.

### 7.1 Gemeinsamer Kern — Level A
Zielprofil pro Stream:
- **48 kHz**, **24 Bit** (L24; L16 optional)
- **≤ 8 Kanäle** pro Stream
- **1 ms Paketzeit**

Das ist exakt die AES67-Pflichtbasis **und** ST-2110-30 Level A. Ein so erzeugter Stream ist maximal interoperabel; ein AES67-Gerät nimmt ihn typischerweise an, ein 2110-Receiver auch.

**Asymmetrie beachten:** Level-A→AES67 klappt praktisch immer, AES67→2110-30 nicht garantiert (z. B. 96 kHz, andere Paketzeiten). Deshalb sendeseitig konservativ auf Level A bleiben, empfangsseitig tolerant parsen.

### 7.2 Was zusätzlich zum Payload rein muss
1. **PTP-Doppelprofil:** sowohl AES67-Media-Profil als auch **SMPTE ST 2059-2** unterstützen (Domain/Parameter nach AES-R16-2016). Umschaltbar, damit die Box in beiden Netzwelten lockt.
2. **Slave-only-Fähigkeit:** In 2110-Netzen ist Slave-only für Mitglieder Pflicht. Der Master-Toggle (§5) muss sich also **hart deaktivieren** lassen, wenn die Box in einem 2110-Netz hängt.
3. **IGMPv3** (mit SSM), abwärtskompatibel zu v2. AES67 verlangt nur v2 — v3 deckt beide ab.
4. **SDP-Clock-Referenz (RFC 7273):** `ts-refclk`/`mediaclk`-Tokens erzeugen und tolerant parsen; unbekannte/„private" Tokens nicht als Fehler werten, sondern ignorieren (so wie AES67-Empfänger es tun).

### 7.3 Ausbaupfad (spätere Stufen, nicht MVP)
- **Level B** — 48 kHz, ≤ 8 ch, **125 µs** Paketzeit → niedrigere Latenz.
- **Level C** — 48 kHz, **bis 64 ch**, 125 µs → MADI-Skalen-Kanalzahlen.
- **AX/BX/CX** — 96 kHz (Kanalzahl halbiert: 4/4/32).
- **ST 2110-31** — AES3-transparent (Dolby E, Nicht-PCM, Daten; aus RAVENNA AM824). Kein AES67-Äquivalent — eigener Payload-Pfad, nur bei echtem Bedarf.

**MVP-Entscheidung:** Nur **Level A** implementieren. B/C/−X/−31 sind additive Erweiterungen auf demselben Fundament, kein Redesign.

---

## 8. Technologie-Stack (Vorschlag)

- **AES67-Engine:** Rust (permissiv, passt zum bestehenden Stack; RTP/SDP/SAP/PTP als testbare Bibliotheken).
- **Virtuelles Gerät:** BlackHole (eingebunden, nicht geforkt — Compose-Ansatz).
- **PTP:** portierte/angebundene 1588-Logik im User-Space; **kein** HW-Timestamping auf macOS erwartet → Software-Servo.
- **Discovery:** SAP (eigen) + mDNS/Bonjour (nativ auf macOS: `dns-sd`/NSNetService — Vorteil).
- **Router:** `nmos-cpp` + `nmos-js`, oder eigenes React-Grid.
- **Web-UI:** React (bekannt), Backend Axum/FastAPI.
- **Packaging:** Developer ID + `notarytool` (bereits aus Wrapper Studio bekannt).

---

## 9. Ehrliche Grenzen und Risiken

Bewusst als eigenes Kapitel — das sind die Stellen, an denen das Projekt scheitern oder überversprechen kann:

1. **macOS hat kein HW-PTP-Timestamping.** Der Endpunkt als *Slave* ist mit Software-PTP + ASRC ok (DVS-Modell). Aber als **netzweiter Grandmaster** ist ein software-getakteter Mac **schwach** — nur für kleine, geschlossene Rigs. Der „richtige" GM gehört auf **Linux/i210, CM4/RK3568 (mit GNSS/PPS)** oder in den **PTP-Switch**. Der Master-Toggle braucht eine Gesundheitswarnung im UI.
2. **Der ASRC/Clock-Recovery-Teil ist empirisch.** Claude Code kann ihn schreiben, aber nicht abhören/tunen — das ist Hardware-in-the-loop-Arbeit auf dem Mac.
3. **Der Core-Audio-Treiber-Loop ist nicht sandbox-testbar.** Deshalb der Compose-Ansatz (BlackHole als fertiges Gerät) statt eigenem Treiber. Ein späterer nativer Fork (Engine im Plugin) ist Stufe 2, mit höherem Risiko.
4. **NMOS steuert nur NMOS-fähige Geräte.** Alt-AES67 braucht die SAP-Bridge — und dort gilt: man kann nur in **kontrollierbare** Receiver pushen (NMOS-IS-05 / eigene Sinks), **nicht** in SAP-only-Fremd-Receiver.
5. **Dante bleibt Fremdwelt.** Discovery via SAP (Dante als Quelle) geht; Routing *in* Dante und AES67-Aktivierung brauchen Dante Controller bzw. die lizenzierte DDM-API. Reverse-Engineering-Tools (`netaudio`/`inferno`) nur als Experiment, nicht als Produktkern.
6. **Interop ist der eigentliche Aufwand**, nicht die API. Von Anfang an gegen das AMWA NMOS Testing Tool validieren.
7. **Medien berühren die Box nie** (außer dem eigenen Endpunkt-Pfad). Wer eine Software-Video-Kreuzschiene erwartet, liegt falsch.

---

## 10. Claude-Code-Arbeitsteilung

**Claude Code trägt (im Loop, testbar):**
- AES67-Engine (RTP/SDP/SAP/PTP-Logik, unit-testbar)
- BlackHole-Anbindung und IPC
- Router-Modul (NMOS-Anbindung, SAP-Bridge, Grid-Backend)
- Web-UI
- Build-System, `notarytool`-Signier-Skripte, CI

**Du besitzt (auf dem Mac, außerhalb der Sandbox):**
- Treiber laden, gegen Pro Tools testen
- ASRC/Clock nach Gehör tunen
- Signier-/Lade-Schleife auf echter Hardware
- PTP-Verhalten im echten Netz mit echtem Switch

Ideale Teilung: Claude schreibt die undifferenzierte Plumbing-Masse, du besitzt die Real-time-/macOS-Validierung.

---

## 11. Phasen-Roadmap

- **Phase 0 — Engine-Sandbox (Linux):** AES67-RTP/SDP/SAP/PTP-Engine gegen PipeWire-AES67 validieren (interoperabel mit Dante/RAVENNA nachweisbar). Rein im Claude-Code-Loop, kein Mac nötig.
- **Phase 1 — macOS-Endpunkt:** BlackHole + Engine + PTP-Slave + ASRC, auf das **Level-A-Zielprofil** (§7: 48 kHz / L24 / 1 ms / ≤8 ch) festgelegt. Ziel: Pro Tools sieht eine AES67-Soundkarte, die driftfrei genug läuft — und deren Streams zugleich ST-2110-30-Level-A-konform sind.
- **Phase 2 — Router-Modul:** NMOS-Registry/Controller + SAP-Bridge + Grid-UI. Gegen Easy-NMOS/Mock-Nodes entwickeln, dann echte Hardware.
- **Phase 3 — PTP-Master-Toggle:** Zustandsmaschine, `priority1`, UI-Warnung. Nur für kleine Rigs freigeben.
- **Phase 4 (später) — ST2110-Video-Steuerung:** derselbe Controller, NMOS deckt Video ohnehin ab; Medien weiterhin nur über den Switch.

---

## 12. Hardware-Kontext (Begleitgeräte)

Passt zur vorangegangenen Analyse:
- **Günstige AES67-Endpunkte:** Pi 5 / RK3568 (NanoPi R5S) + `aes67-linux-daemon` bzw. PipeWire-AES67; ESP32-P4 fürs Stereo-Fixgerät.
- **Echter Grandmaster** (wenn nötig): CM4/CM5 oder RK3568 mit GNSS/1PPS (`ts2phc` + `ptp4l`), oder PTP-Switch. **Nicht** der Mac.
- **Switch:** PTP-fähig (Boundary/Transparent Clock), IGMP-Snooping — der größte Qualitätshebel im Netz.

---

*Ende des Briefs. Offene Basis, kein Dante, keine Lizenz — Bedienung selbst gebaut. Die drei Rollen (Endpunkt / Router / Master) sind ein Kern mit zwei zuschaltbaren Modulen, streng isoliert, mit der PTP-Rolle als bewusstem Umschalter statt additivem Feature.*
