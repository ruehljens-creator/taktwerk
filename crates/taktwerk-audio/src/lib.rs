//! # taktwerk-audio
//!
//! Die **OS-Naht fuer das virtuelle Audiogeraet** (§3.1/§4 des Projektbriefs).
//! Der Rest des Systems kennt nur den [`AudioBackend`]-Trait — nie Core Audio,
//! PipeWire oder WASAPI direkt. Damit ist der Endpunkt-Pfad plattformneutral
//! verdrahtet; die konkreten Geraete-Backends werden per Cargo-Feature/`cfg`
//! nur auf der jeweiligen Plattform eingezogen.
//!
//! ## Phasen (siehe README / Projektbrief §11)
//! - **Phase 0 (jetzt):** nur [`NullBackend`] — kein Geraet, reine Netz-/Datei-
//!   Verarbeitung. Baut und laeuft identisch auf Linux, macOS **und** Windows.
//! - **Phase 1:** `macos-blackhole` (BlackHole via Core Audio, Subprozess →
//!   keine GPL-Verlinkung, §4) und `linux-pipewire`. Windows-Virtualgeraet
//!   bleibt vorerst offen (kein freier Treiber) — der Trait steht bereit.
//!
//! Warum ein Trait und kein `enum`: Backends bringen unterschiedliche, teils
//! plattform-only Abhaengigkeiten mit. Ein Trait-Objekt haelt den Aufrufer
//! (Daemon) frei von `cfg`-Streuung; die Auswahl passiert einmal in [`open_default`].

use taktwerk_core::StreamProfile;

pub mod asrc;

#[cfg(feature = "cpal-backend")]
pub mod cpal_backend;
#[cfg(feature = "cpal-backend")]
pub use cpal_backend::{list_devices, CpalBackend};

/// Fehler beim Oeffnen/Betreiben eines Audio-Backends.
#[derive(Debug)]
pub enum AudioError {
    /// Auf dieser Plattform / mit diesen Features nicht verfuegbar.
    Unavailable(&'static str),
    /// Geraet/Backend konnte nicht geoeffnet werden.
    Open(String),
}

impl core::fmt::Display for AudioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            AudioError::Unavailable(s) => write!(f, "Audio-Backend nicht verfuegbar: {s}"),
            AudioError::Open(s) => write!(f, "Audio-Backend Oeffnen fehlgeschlagen: {s}"),
        }
    }
}

impl std::error::Error for AudioError {}

/// Ein Block interleavter Samples (i32, linksbuendig — vgl. `rtp::decode_payload`).
pub type AudioBlock = Vec<i32>;

/// Die plattformneutrale Sicht auf ein virtuelles Audiogeraet.
///
/// Der Endpunkt liest aufgenommene Samples (DAW → Geraet → Netz) und schreibt
/// empfangene Samples (Netz → Geraet → DAW). Implementierungen kapseln die
/// Clock-Domaene des Geraets; die Bruecke zum Netztakt macht der ASRC-Servo
/// (`taktwerk_core::dsp`).
pub trait AudioBackend: Send {
    /// Menschlich lesbarer Backend-Name (fuer UI/Logs).
    fn name(&self) -> &str;

    /// Das aktuell betriebene Stream-Profil (Rate/Kanaele/Encoding).
    fn profile(&self) -> StreamProfile;

    /// Liest bis zu `max_frames` aufgenommene Frames aus dem Geraet (Richtung
    /// DAW → Netz). Gibt die tatsaechlich gelesenen Samples zurueck
    /// (Laenge = frames * channels), ggf. leer.
    fn read_capture(&mut self, max_frames: usize) -> Result<AudioBlock, AudioError>;

    /// Schreibt empfangene Samples ins Geraet (Richtung Netz → DAW).
    fn write_playback(&mut self, samples: &[i32]) -> Result<(), AudioError>;
}

/// Headless-Backend fuer Phase 0: **kein echtes Geraet**. Capture liefert Stille
/// (bzw. optional ein injizierbares Testsignal), Playback wird verworfen bzw.
/// nur mitgezaehlt. Erlaubt es, die komplette Netz-/Protokoll-Kette auf jeder
/// Plattform ohne Treiber zu testen.
pub struct NullBackend {
    profile: StreamProfile,
    frames_written: u64,
}

impl NullBackend {
    pub fn new(profile: StreamProfile) -> Self {
        Self {
            profile,
            frames_written: 0,
        }
    }

    /// Wie viele Frames insgesamt zur Wiedergabe geschrieben wurden (Test-Metrik).
    pub fn frames_written(&self) -> u64 {
        self.frames_written
    }
}

impl AudioBackend for NullBackend {
    fn name(&self) -> &str {
        "null (headless)"
    }

    fn profile(&self) -> StreamProfile {
        self.profile
    }

    fn read_capture(&mut self, max_frames: usize) -> Result<AudioBlock, AudioError> {
        // Stille in der geforderten Blockgroesse.
        Ok(vec![0i32; max_frames * self.profile.channels as usize])
    }

    fn write_playback(&mut self, samples: &[i32]) -> Result<(), AudioError> {
        let ch = self.profile.channels as u64;
        if let Some(frames) = (samples.len() as u64).checked_div(ch) {
            self.frames_written += frames;
        }
        Ok(())
    }
}

/// Oeffnet das fuer die aktuelle Plattform/Features passende Backend.
/// In Phase 0 immer der [`NullBackend`]; ab Phase 1 waehlt diese Funktion
/// per `cfg`/Feature das echte Geraete-Backend und faellt sonst auf headless.
pub fn open_default(profile: StreamProfile) -> Result<Box<dyn AudioBackend>, AudioError> {
    // Platzhalter fuer Phase 1 — Struktur steht, Auswahl ist zentral:
    #[cfg(all(target_os = "macos", feature = "macos-blackhole"))]
    {
        // return backends::macos::BlackHoleBackend::open(profile).map(...);
    }
    #[cfg(all(target_os = "linux", feature = "linux-pipewire"))]
    {
        // return backends::linux::PipeWireBackend::open(profile).map(...);
    }
    Ok(Box::new(NullBackend::new(profile)))
}

/// Gewünschtes Audiogerät je Richtung. `None` = Standard-Gerät der Plattform;
/// `Some(name)` wählt gezielt per Name (exakt, sonst Teilstring — z. B.
/// „Pro Tools Audio Bridge"). Nur relevant mit `use_device` + `cpal-backend`.
#[derive(Debug, Default, Clone)]
pub struct DeviceSelection {
    /// Aufnahmegerät (TX: DAW → Netz).
    pub capture: Option<String>,
    /// Wiedergabegerät (RX: Netz → DAW).
    pub playback: Option<String>,
}

/// Öffnet ein Backend für die gewünschten Richtungen mit Standard-Geräten.
/// Bequemer Wrapper um [`open_with`] ohne gezielte Gerätewahl.
pub fn open(
    profile: StreamProfile,
    capture: bool,
    playback: bool,
    use_device: bool,
) -> Box<dyn AudioBackend> {
    open_with(
        profile,
        capture,
        playback,
        use_device,
        DeviceSelection::default(),
    )
}

/// Öffnet ein Backend für die gewünschten Richtungen. Wenn `use_device` gesetzt
/// **und** das `cpal-backend`-Feature aktiv ist, wird ein echtes Gerät versucht
/// (ggf. per Name aus `sel` gewählt); gelingt das nicht, fällt es sauber auf
/// [`NullBackend`] zurück. Ohne Feature/`use_device` immer headless — der
/// Standardpfad bleibt unberührt.
pub fn open_with(
    profile: StreamProfile,
    _capture: bool,
    _playback: bool,
    use_device: bool,
    _sel: DeviceSelection,
) -> Box<dyn AudioBackend> {
    #[cfg(feature = "cpal-backend")]
    if use_device {
        match CpalBackend::with_devices(profile, _capture, _playback, _sel.capture, _sel.playback) {
            Ok(b) => {
                tracing::info!(ch = profile.channels, "cpal-Audiogerät geöffnet");
                return Box::new(b);
            }
            Err(e) => tracing::warn!("cpal-Gerät nicht verfügbar ({e}) — NullBackend"),
        }
    }
    let _ = use_device;
    Box::new(NullBackend::new(profile))
}

#[cfg(test)]
mod tests {
    use super::*;
    use taktwerk_core::StreamProfile;

    #[test]
    fn null_backend_capture_size_matches_profile() {
        let mut b = NullBackend::new(StreamProfile::level_a(8));
        let block = b.read_capture(48).unwrap();
        assert_eq!(block.len(), 48 * 8);
        assert!(block.iter().all(|&s| s == 0));
    }

    #[test]
    fn null_backend_counts_written_frames() {
        let mut b = NullBackend::new(StreamProfile::level_a(2));
        b.write_playback(&[0; 48 * 2]).unwrap();
        assert_eq!(b.frames_written(), 48);
    }

    #[test]
    fn open_default_is_headless_in_phase0() {
        let b = open_default(StreamProfile::level_a(2)).unwrap();
        assert_eq!(b.name(), "null (headless)");
    }
}
