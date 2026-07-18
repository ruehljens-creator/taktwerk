//! ASRC-Anbindung auf der **Wiedergabe-Seite**: der plattformneutrale PI-Servo
//! aus [`taktwerk_core::dsp::AsrcServo`] plus der eigentliche Sample-Rate-Wandler.
//!
//! Aufgabe (§6 des Projektbriefs): die beiden Clock-Domänen überbrücken — der
//! Netz-/PTP-Takt liefert Samples in den Jitter-Puffer, der **Geräte**-Takt
//! (cpal-Callback) leert ihn. Beide driften minimal auseinander; ohne Ausgleich
//! läuft der Puffer über (Latenz wächst → Verwerfen) oder leer (Knackser). Der
//! Servo misst den Füllstandsfehler und der Resampler zieht die Eingabe um ein
//! winziges Verhältnis (~±ppm) nach, so dass der Füllstand am Ziel bleibt.
//!
//! Der Resampler ist bewusst ein **linearer Interpolator**: bei Korrekturen von
//! ≤ 2000 ppm (harte Servo-Klammer) sind seine Artefakte weit unter der
//! Hörschwelle. Ein Polyphasen-Filter ist eine spätere Verfeinerung.

use taktwerk_core::dsp::AsrcServo;

/// Frame-Wert eines virtuellen Eingangs: Index 0 = `hist` (letztes Frame des
/// vorherigen Blocks), Index k≥1 = `input[k-1]`. So bleibt die Interpolation über
/// Blockgrenzen hinweg lückenlos.
#[inline]
fn frame_at(k: usize, hist: &[i32], input: &[i32], ch: usize, c: usize) -> f64 {
    if k == 0 {
        hist[c] as f64
    } else {
        input[(k - 1) * ch + c] as f64
    }
}

/// Streaming-Linear-Resampler (zustandsbehaftet über Blockgrenzen).
#[derive(Debug, Clone)]
pub struct LinearResampler {
    channels: usize,
    /// Fraktionale Leseposition im virtuellen Eingang (siehe [`frame_at`]).
    pos: f64,
    /// Letztes Eingabe-Frame (je Kanal) für die Interpolation zum nächsten Block.
    hist: Vec<i32>,
}

impl LinearResampler {
    pub fn new(channels: usize) -> Self {
        Self {
            channels,
            pos: 0.0,
            hist: vec![0; channels.max(1)],
        }
    }

    /// Resampled `input` (interleaved, `channels`) mit `ratio` und hängt das
    /// Ergebnis an `out` an. Ausgabe ≈ `input_frames / ratio` Frames: `ratio > 1`
    /// erzeugt **weniger** Samples (Puffer zu voll → drosseln), `ratio < 1` mehr.
    pub fn process(&mut self, input: &[i32], ratio: f64, out: &mut Vec<i32>) {
        let ch = self.channels;
        if ch == 0 || input.len() < ch {
            return;
        }
        let frames = input.len() / ch;
        while self.pos < frames as f64 {
            let base = self.pos.floor() as usize;
            let frac = self.pos - base as f64;
            for c in 0..ch {
                let a = frame_at(base, &self.hist, input, ch, c);
                let b = frame_at(base + 1, &self.hist, input, ch, c);
                out.push((a + (b - a) * frac).round() as i32);
            }
            self.pos += ratio;
        }
        // Restposition auf den nächsten Block umrechnen (dessen virtueller Index 0
        // das jetzt letzte Eingabe-Frame ist).
        self.pos -= frames as f64;
        for c in 0..ch {
            self.hist[c] = input[(frames - 1) * ch + c];
        }
    }
}

/// Servo + Resampler + Ziel-Füllstand: der komplette ASRC für einen Ausgabe-Puffer.
#[derive(Debug, Clone)]
pub struct Asrc {
    servo: AsrcServo,
    resampler: LinearResampler,
    /// Ziel-Füllstand des Jitter-Puffers in Frames.
    target_frames: usize,
}

impl Asrc {
    /// Default-Ziel ~10 ms Jitter-Puffer bei gegebener Abtastrate (TUNE an HW).
    pub fn new(channels: usize, sample_rate: u32) -> Self {
        let target_frames = (sample_rate as usize / 100).max(2); // 10 ms
        Self {
            servo: AsrcServo::default_level_a(),
            resampler: LinearResampler::new(channels),
            target_frames,
        }
    }

    /// Ziel-Füllstand (Frames), den der Prime-Gate vor dem ersten Auslesen anstrebt.
    pub fn target_frames(&self) -> usize {
        self.target_frames
    }

    /// Regelt anhand des aktuellen Füllstands (`fill_frames`) und resampled den
    /// Eingabe-Block in `out`.
    pub fn process(&mut self, fill_frames: usize, input: &[i32], out: &mut Vec<i32>) {
        let err = fill_frames as f64 - self.target_frames as f64;
        let ratio = self.servo.update(err);
        self.resampler.process(input, ratio, out);
    }

    /// Aktuelles Resampling-Verhältnis (für Logs/Diagnose).
    pub fn ratio(&self) -> f64 {
        self.servo.ratio()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_ratio_is_near_identity() {
        let mut r = LinearResampler::new(1);
        let input: Vec<i32> = (0..100).map(|i| i * 1000).collect();
        let mut out = Vec::new();
        r.process(&input, 1.0, &mut out);
        // Bei ratio=1 ~ gleiche Frameanzahl (±1 durch Phasen-Carry).
        assert!(
            (out.len() as i64 - input.len() as i64).abs() <= 1,
            "out {} vs in {}",
            out.len(),
            input.len()
        );
    }

    #[test]
    fn ratio_above_one_yields_fewer_samples() {
        let mut r = LinearResampler::new(2);
        let input = vec![0i32; 480 * 2]; // 480 Frames stereo
        let mut out = Vec::new();
        r.process(&input, 1.10, &mut out);
        let out_frames = out.len() / 2;
        assert!(out_frames < 480, "erwartet < 480, war {out_frames}");
    }

    #[test]
    fn ratio_below_one_yields_more_samples() {
        let mut r = LinearResampler::new(2);
        let input = vec![0i32; 480 * 2];
        let mut out = Vec::new();
        r.process(&input, 0.90, &mut out);
        let out_frames = out.len() / 2;
        assert!(out_frames > 480, "erwartet > 480, war {out_frames}");
    }

    #[test]
    fn overfull_buffer_downsamples_toward_target() {
        let mut a = Asrc::new(2, 48_000);
        let target = a.target_frames();
        // Puffer deutlich über Ziel → ratio > 1 → weniger Ausgabe als Eingabe.
        let input = vec![0i32; 48 * 2];
        let mut out = Vec::new();
        a.process(target + 1000, &input, &mut out);
        assert!(a.ratio() > 1.0, "ratio {} sollte > 1 sein", a.ratio());
        assert!(out.len() <= input.len(), "sollte nicht mehr Samples erzeugen");
    }
}
