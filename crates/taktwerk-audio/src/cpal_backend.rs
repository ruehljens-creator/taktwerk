//! Portables Echt-Geräte-Backend über **cpal** (WASAPI · CoreAudio · ALSA).
//!
//! cpal liefert Audio über **Callbacks in einem eigenen Thread**; der
//! `AudioBackend`-Trait ist dagegen synchron (pull/push). Die Brücke sind zwei
//! Ring-Puffer: der Input-Callback schiebt aufgenommene Samples hinein
//! ([`CpalBackend::read_capture`] holt sie), der Output-Callback zieht
//! Wiedergabe-Samples heraus ([`CpalBackend::write_playback`] füllt sie).
//!
//! cpal-Streams sind teils `!Send`; deshalb leben sie in einem dedizierten
//! Thread, während der Backend-Struct nur die (Send-)Puffer hält.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use taktwerk_core::StreamProfile;

use crate::{AudioBackend, AudioBlock, AudioError};

/// f32 (−1..1) → i32 linksbündig (Nutzsignal in den oberen Bits).
#[inline]
fn f32_to_i32(s: f32) -> i32 {
    (s.clamp(-1.0, 1.0) as f64 * 2_147_483_647.0) as i32
}
/// i32 linksbündig → f32 (−1..1).
#[inline]
fn i32_to_f32(s: i32) -> f32 {
    (s as f64 / 2_147_483_648.0) as f32
}

/// Maximale Puffertiefe (1 s), damit Über-/Unterlauf nicht unbegrenzt wächst.
fn max_samples(profile: &StreamProfile) -> usize {
    profile.sample_rate as usize * profile.channels as usize
}

/// Sammelt die Namen aus einem (fehlbaren) Geräte-Iterator.
fn collect_names<I>(it: Result<I, cpal::DevicesError>) -> Vec<String>
where
    I: Iterator<Item = cpal::Device>,
{
    it.map(|devs| devs.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Listet die Namen der verfügbaren Ein- und Ausgabegeräte (für Diagnose/UI).
pub fn list_devices() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    (
        collect_names(host.input_devices()),
        collect_names(host.output_devices()),
    )
}

/// Lesbarer Richtungsname für Fehlermeldungen.
fn dir_label(input: bool) -> &'static str {
    if input {
        "Eingabegerät"
    } else {
        "Ausgabegerät"
    }
}

/// Wählt ein cpal-Gerät für die Richtung aus.
///
/// - `name == None` → Default-Gerät der Richtung.
/// - `name == Some(x)` → erst **exakter** (case-insensitiver) Namenstreffer,
///   sonst erster **Teilstring**-Treffer (z. B. `"Pro Tools"` findet
///   „Pro Tools Audio Bridge 2-A"). Kein Treffer ist ein **Fehler** (der
///   Aufrufer entscheidet dann bewusst über einen Fallback) — so landet eine
///   gezielte Gerätewahl nie stillschweigend auf dem falschen (Default-)Gerät.
fn pick_device(host: &cpal::Host, name: Option<&str>, input: bool) -> Result<cpal::Device, String> {
    let want = match name {
        None => {
            return if input {
                host.default_input_device()
            } else {
                host.default_output_device()
            }
            .ok_or_else(|| format!("kein {}", dir_label(input)));
        }
        Some(w) => w,
    };

    let devices: Vec<cpal::Device> = if input {
        host.input_devices()
    } else {
        host.output_devices()
    }
    .map_err(|e| e.to_string())?
    .collect();

    // 1) exakter Treffer (case-insensitiv)
    if let Some(d) = devices
        .iter()
        .find(|d| d.name().is_ok_and(|n| n.eq_ignore_ascii_case(want)))
    {
        return Ok(d.clone());
    }
    // 2) Teilstring (case-insensitiv)
    let want_low = want.to_lowercase();
    if let Some(d) = devices
        .iter()
        .find(|d| d.name().is_ok_and(|n| n.to_lowercase().contains(&want_low)))
    {
        return Ok(d.clone());
    }
    Err(format!("kein {} mit Name ~ \"{want}\"", dir_label(input)))
}

/// Echt-Geräte-Backend. `capture`/`playback` wählen die genutzten Richtungen.
pub struct CpalBackend {
    profile: StreamProfile,
    capture_buf: Arc<Mutex<VecDeque<i32>>>,
    playback_buf: Arc<Mutex<VecDeque<i32>>>,
    frames_written: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl CpalBackend {
    /// Öffnet die **Default**-Geräte für die gewünschten Richtungen. Schlägt fehl
    /// (→ Aufrufer kann auf `NullBackend` zurückfallen), wenn kein Gerät da ist.
    pub fn new(profile: StreamProfile, capture: bool, playback: bool) -> Result<Self, AudioError> {
        Self::with_devices(profile, capture, playback, None, None)
    }

    /// Wie [`Self::new`], aber mit **gezielter Gerätewahl per Name**
    /// (`capture_name`/`playback_name`, siehe [`pick_device`]). `None` = Default.
    /// Damit lässt sich z. B. „Pro Tools Audio Bridge" als AES67↔DAW-Brücke
    /// wählen, statt das System-Default-Mikrofon zu nehmen.
    pub fn with_devices(
        profile: StreamProfile,
        capture: bool,
        playback: bool,
        capture_name: Option<String>,
        playback_name: Option<String>,
    ) -> Result<Self, AudioError> {
        let capture_buf = Arc::new(Mutex::new(VecDeque::<i32>::new()));
        let playback_buf = Arc::new(Mutex::new(VecDeque::<i32>::new()));
        let frames_written = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let (cb, pb, fw, st) = (
            capture_buf.clone(),
            playback_buf.clone(),
            frames_written.clone(),
            stop.clone(),
        );
        // Streams im eigenen Thread bauen & am Leben halten (cpal-Streams !Send).
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let thread = std::thread::spawn(move || {
            match build_streams(profile, capture, playback, capture_name, playback_name, cb, pb, fw) {
                Ok(streams) => {
                    let _ = ready_tx.send(Ok(()));
                    while !st.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(streams); // Streams schließen
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                profile,
                capture_buf,
                playback_buf,
                frames_written,
                stop,
                _thread: Some(thread),
            }),
            Ok(Err(e)) => Err(AudioError::Open(e)),
            Err(_) => Err(AudioError::Open("cpal-Thread abgebrochen".into())),
        }
    }
}

impl Drop for CpalBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self._thread.take() {
            let _ = t.join();
        }
    }
}

impl AudioBackend for CpalBackend {
    fn name(&self) -> &str {
        "cpal (device)"
    }
    fn profile(&self) -> StreamProfile {
        self.profile
    }

    fn read_capture(&mut self, max_frames: usize) -> Result<AudioBlock, AudioError> {
        let want = max_frames * self.profile.channels as usize;
        let mut buf = self.capture_buf.lock().unwrap();
        let mut out = Vec::with_capacity(want);
        for _ in 0..want {
            out.push(buf.pop_front().unwrap_or(0)); // Unterlauf → Stille
        }
        Ok(out)
    }

    fn write_playback(&mut self, samples: &[i32]) -> Result<(), AudioError> {
        let ch = self.profile.channels as u64;
        {
            let mut buf = self.playback_buf.lock().unwrap();
            buf.extend(samples.iter().copied());
            // Überlauf begrenzen (älteste verwerfen).
            let cap = max_samples(&self.profile);
            while buf.len() > cap {
                buf.pop_front();
            }
        }
        if ch > 0 {
            self.frames_written
                .fetch_add(samples.len() as u64 / ch, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Baut die gewünschten cpal-Streams (im cpal-Thread aufzurufen).
#[allow(clippy::too_many_arguments)]
fn build_streams(
    profile: StreamProfile,
    capture: bool,
    playback: bool,
    capture_name: Option<String>,
    playback_name: Option<String>,
    capture_buf: Arc<Mutex<VecDeque<i32>>>,
    playback_buf: Arc<Mutex<VecDeque<i32>>>,
    _frames_written: Arc<AtomicU64>,
) -> Result<Vec<cpal::Stream>, String> {
    let host = cpal::default_host();
    let mut streams = Vec::new();
    let config = cpal::StreamConfig {
        channels: profile.channels as u16,
        sample_rate: cpal::SampleRate(profile.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let cap_max = max_samples(&profile);
    let err_cb = |e| tracing::warn!("cpal-Stream-Fehler: {e}");

    if capture {
        let dev = pick_device(&host, capture_name.as_deref(), true)?;
        tracing::info!(dev = dev.name().unwrap_or_default(), "cpal Capture-Gerät");
        let fmt = dev
            .default_input_config()
            .map_err(|e| e.to_string())?
            .sample_format();
        let buf = capture_buf.clone();
        let stream = match fmt {
            SampleFormat::F32 => dev.build_input_stream(
                &config,
                move |data: &[f32], _| {
                    let mut b = buf.lock().unwrap();
                    for &s in data {
                        b.push_back(f32_to_i32(s));
                    }
                    while b.len() > cap_max {
                        b.pop_front();
                    }
                },
                err_cb,
                None,
            ),
            SampleFormat::I16 => dev.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let mut b = buf.lock().unwrap();
                    for &s in data {
                        b.push_back((s as i32) << 16);
                    }
                    while b.len() > cap_max {
                        b.pop_front();
                    }
                },
                err_cb,
                None,
            ),
            other => return Err(format!("Eingabe-Sampleformat nicht unterstützt: {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        streams.push(stream);
    }

    if playback {
        let dev = pick_device(&host, playback_name.as_deref(), false)?;
        tracing::info!(dev = dev.name().unwrap_or_default(), "cpal Playback-Gerät");
        let fmt = dev
            .default_output_config()
            .map_err(|e| e.to_string())?
            .sample_format();
        let buf = playback_buf.clone();
        let stream = match fmt {
            SampleFormat::F32 => dev.build_output_stream(
                &config,
                move |data: &mut [f32], _| {
                    let mut b = buf.lock().unwrap();
                    for slot in data.iter_mut() {
                        *slot = i32_to_f32(b.pop_front().unwrap_or(0));
                    }
                },
                err_cb,
                None,
            ),
            SampleFormat::I16 => dev.build_output_stream(
                &config,
                move |data: &mut [i16], _| {
                    let mut b = buf.lock().unwrap();
                    for slot in data.iter_mut() {
                        *slot = (b.pop_front().unwrap_or(0) >> 16) as i16;
                    }
                },
                err_cb,
                None,
            ),
            other => return Err(format!("Ausgabe-Sampleformat nicht unterstützt: {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        streams.push(stream);
    }

    Ok(streams)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_i32_roundtrip_is_close() {
        for &x in &[-1.0f32, -0.5, 0.0, 0.5, 0.999] {
            let back = i32_to_f32(f32_to_i32(x));
            assert!((back - x).abs() < 1e-6, "x={x} back={back}");
        }
    }

    #[test]
    fn f32_clamps_out_of_range() {
        assert_eq!(f32_to_i32(2.0), f32_to_i32(1.0));
        assert_eq!(f32_to_i32(-2.0), f32_to_i32(-1.0));
    }

    #[test]
    fn max_samples_is_one_second() {
        let p = StreamProfile::level_a(2);
        assert_eq!(max_samples(&p), 96_000);
    }

    #[test]
    fn dir_label_reads_naturally() {
        assert_eq!(dir_label(true), "Eingabegerät");
        assert_eq!(dir_label(false), "Ausgabegerät");
    }

    #[test]
    fn pick_device_unknown_name_is_error() {
        // Ein absichtlich unmöglicher Name darf nie ein (falsches) Default-Gerät
        // liefern — gezielte Wahl schlägt sichtbar fehl statt still daneben.
        let host = cpal::default_host();
        let ok = pick_device(&host, Some("::kein-solches-gerät-4711::"), true)
            .map(|d| d.name().unwrap_or_default());
        assert!(ok.is_err(), "unbekannter Name müsste Err sein, war {ok:?}");
    }
}
