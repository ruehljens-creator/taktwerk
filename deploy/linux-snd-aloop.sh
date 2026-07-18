#!/usr/bin/env bash
# Richtet auf einem headless-Linux (Debian/Ubuntu, o. Ä.) eine **virtuelle
# AES67-Soundkarte** über den ALSA-Kernel-Loopback `snd-aloop` ein — ohne
# PipeWire/PulseAudio. Danach präsentiert der Node den Netz-Audio-Pfad als
# System-Audiogerät: was eine App in die Karte spielt, geht als AES67 raus (TX);
# empfangenes AES67 taucht als aufnehmbares Gerät auf (RX).
#
# snd-aloop ist eine strikte Kabel-Bruecke: beide Enden müssen dieselbe Rate/
# Kanalzahl/Format nutzen — Taktwerk fährt AES67-konform 48 kHz.
#
# Nutzung:  sudo deploy/linux-snd-aloop.sh
set -euo pipefail

if [ "$(id -u)" != 0 ]; then
  echo "Bitte als root ausführen (sudo)." >&2
  exit 1
fi

# 1) Build-/Laufzeit-Abhängigkeiten des cpal-Backends (ALSA).
if command -v apt-get >/dev/null 2>&1; then
  apt-get install -y libasound2-dev pkg-config
else
  echo "Hinweis: libasound2-dev + pkg-config selbst installieren (kein apt gefunden)."
fi

# 2) Kernel-Loopback laden (Karte 'Loopback', 8 Substreams je Richtung).
modprobe snd-aloop index=2 pcm_substreams=8 id=Loopback

# 3) Persistent über Reboots.
echo snd-aloop > /etc/modules-load.d/taktwerk-snd-aloop.conf
echo "options snd-aloop index=2 pcm_substreams=8 id=Loopback" \
  > /etc/modprobe.d/taktwerk-snd-aloop.conf

echo
echo "OK — ALSA-Karte 'Loopback' bereit (prüfen: cat /proc/asound/cards)."
echo "Selbsttest (Ton fließt durch das virtuelle Gerät in den Aufnahmepfad):"
echo "  cargo run -p taktwerk-audio --features cpal-backend --example loopback_flow \\"
echo "    -- Loopback,DEV=1 Loopback,DEV=0"
echo
echo "Daemon-Nutzung:"
echo "  TAKTWERK_AUDIO=cpal TAKTWERK_AUDIO_IN=Loopback  taktwerkd   # App -> AES67 (TX)"
echo "  TAKTWERK_AUDIO=cpal TAKTWERK_AUDIO_OUT=Loopback taktwerkd   # AES67 -> App (RX)"
echo "Snd-aloop-Kabelung: was auf Loopback,DEV=0 gespielt wird, erscheint auf DEV=1 (und umgekehrt)."
