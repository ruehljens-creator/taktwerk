//! # taktwerk-endpoint
//!
//! Die **Media-Loop** des AES67-Endpunkts — die Stelle, an der der plattform-
//! neutrale Kern, das Audio-Backend und die Netzschicht zusammenlaufen:
//!
//! - [`tx::TxStream`] — liest Samples aus einem [`AudioBackend`] und schickt sie
//!   im Paket-Takt (ptime) als RTP-Stream ins Netz (Capture → Netz).
//! - [`rx::RxStream`] — empfängt RTP-Pakete und schreibt die Samples ins
//!   [`AudioBackend`] (Netz → Playback).
//!
//! Beide sind bewusst backend-agnostisch: In Phase 0 laufen sie mit dem
//! `NullBackend` (headless), in Phase 1 mit BlackHole/PipeWire — **ohne**
//! Änderung an dieser Loop. Der Start-RTP-Timestamp kommt aus einer
//! [`taktwerk_core::clock::TimeSource`], der Fortlauf aus dem RTP-Sender.
//!
//! [`AudioBackend`]: taktwerk_audio::AudioBackend

pub mod rx;
pub mod tx;

pub use rx::RxStream;
pub use tx::TxStream;

use std::io;

/// Wandelt einen Audio-Backend-Fehler in einen `io::Error` (gemeinsamer
/// Fehlertyp der Loop).
pub(crate) fn audio_err(e: taktwerk_audio::AudioError) -> io::Error {
    io::Error::other(e)
}
