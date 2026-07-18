//! ASRC / Clock-Recovery-Servo — die kritische Bruecke zwischen den beiden
//! Clock-Domaenen (§6 des Projektbriefs): Core-Audio-Geraetetakt (BlackHole) vs.
//! PTP-Netztakt.
//!
//! **Wichtiger Vorbehalt (§9.2):** Die eigentliche ASRC-Auslegung ist empirisch
//! und muss auf echter Hardware nach Gehoer getunt werden — das kann diese Lib
//! nicht leisten. Hier lebt nur der **plattformneutrale, testbare Regel-Teil**:
//! ein PI-Servo, der aus der Fuellstands-/Phasenabweichung eines Ringpuffers ein
//! Resampling-Verhaeltnis nachfuehrt. Der eigentliche Sample-Rate-Wandler
//! (Polyphase-Filter) und die Anbindung an das Audiogeraet kommen in
//! `taktwerk-audio`.

/// Ein einfacher PI-Regler, der ein Resampling-Verhaeltnis um 1.0 herum fuehrt.
///
/// Eingang ist der **Fuellstandsfehler** des Jitter-Puffers (Ist − Soll, in
/// Frames): positiv = Puffer laeuft voll → schneller auslesen (ratio > 1);
/// negativ = Puffer leert → langsamer (ratio < 1). Ausgang ist das an den SRC
/// zu uebergebende Verhaeltnis, hart auf einen kleinen Korridor begrenzt, damit
/// nie hoerbare Tonhoehen-Spruenge entstehen.
#[derive(Debug, Clone)]
pub struct AsrcServo {
    kp: f64,
    ki: f64,
    integral: f64,
    /// Maximale relative Abweichung von 1.0 (z. B. 0.002 = 2000 ppm).
    max_deviation: f64,
    ratio: f64,
}

impl AsrcServo {
    /// `kp`/`ki`: Regler-Gains (TUNE, an Hardware justieren). `max_deviation`:
    /// harte Klammer um 1.0 (z. B. 0.002).
    pub fn new(kp: f64, ki: f64, max_deviation: f64) -> Self {
        Self {
            kp,
            ki,
            integral: 0.0,
            max_deviation,
            ratio: 1.0,
        }
    }

    /// Konservativer Default fuer den Level-A-Endpunkt (sanft, ±2000 ppm).
    pub fn default_level_a() -> Self {
        Self::new(1.0e-6, 5.0e-8, 0.002)
    }

    /// Fuehrt einen Regelschritt aus und gibt das begrenzte Resampling-Verhaeltnis
    /// zurueck. `fill_error_frames` = aktueller Fuellstand − Zielfuellstand.
    pub fn update(&mut self, fill_error_frames: f64) -> f64 {
        self.integral += fill_error_frames;
        // Anti-Windup: Integral im Rahmen halten.
        let integ_clamp = self.max_deviation / self.ki.max(f64::MIN_POSITIVE);
        self.integral = self.integral.clamp(-integ_clamp, integ_clamp);

        let correction = self.kp * fill_error_frames + self.ki * self.integral;
        self.ratio = (1.0 + correction).clamp(1.0 - self.max_deviation, 1.0 + self.max_deviation);
        self.ratio
    }

    /// Aktuelles Verhaeltnis ohne neuen Schritt.
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Servo-Reset (Rollenwechsel Slave↔Master, §5 → ASRC neu einschwingen).
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.ratio = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_unity() {
        let s = AsrcServo::default_level_a();
        assert_eq!(s.ratio(), 1.0);
    }

    #[test]
    fn positive_fill_speeds_up() {
        let mut s = AsrcServo::default_level_a();
        let r = s.update(500.0); // Puffer zu voll
        assert!(r > 1.0, "ratio {r} sollte > 1 sein");
    }

    #[test]
    fn negative_fill_slows_down() {
        let mut s = AsrcServo::default_level_a();
        let r = s.update(-500.0);
        assert!(r < 1.0, "ratio {r} sollte < 1 sein");
    }

    #[test]
    fn ratio_is_hard_clamped() {
        let mut s = AsrcServo::new(1.0, 1.0, 0.002);
        let r = s.update(1.0e9); // absurd grosser Fehler
        assert!(
            (r - 1.002).abs() < 1e-9,
            "ratio {r} muss auf +max_deviation klemmen"
        );
    }

    #[test]
    fn reset_returns_to_unity() {
        let mut s = AsrcServo::default_level_a();
        s.update(1000.0);
        s.reset();
        assert_eq!(s.ratio(), 1.0);
        assert_eq!(s.integral, 0.0);
    }

    #[test]
    fn converges_toward_target_over_time() {
        // Simuliere einen Puffer, der bei ratio>1 schneller leert: grober
        // Regelkreis-Sanity-Check, dass der Servo den Fehler nicht aufschaukelt.
        let mut s = AsrcServo::default_level_a();
        let mut fill = 800.0;
        for _ in 0..2000 {
            let ratio = s.update(fill);
            // ratio>1 → mehr Auslesen → Fuellstand sinkt (vereinfachtes Modell)
            fill -= (ratio - 1.0) * 1.0e6;
        }
        assert!(
            fill.abs() < 800.0,
            "Fuellstandsfehler {fill} sollte kleiner geworden sein"
        );
    }
}
