//! PTP-Servo (Offset-/Delay-Mathematik) + [`PtpTimeSource`].
//!
//! Der Servo rechnet aus Sync/Follow_Up- und Delay-Req/Resp-Zeitpaaren den
//! Offset zur Master-Uhr und die mittlere Pfad-Verzögerung — reine, testbare
//! Arithmetik (IEEE 1588, 11.2/11.3). Die eigentlichen Zeitstempel kommen vom
//! Netz-Client; hier ist nur die Regel-Logik.
//!
//! [`PtpTimeSource`] verbindet das Ganze mit der [`crate::clock::TimeSource`]-
//! Naht: Es liest eine lokale Uhr und korrigiert sie um den (vom Servo gepflegten)
//! Offset → liefert Netz-/Master-Zeit für RTP-Timestamps. Der Offset wird über
//! einen `Arc<AtomicI64>` geteilt, damit der Netz-Client ihn live nachführt,
//! während der Endpunkt die Zeit liest.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use crate::clock::TimeSource;

/// PTP-Servo: pflegt Offset (Slave − Master) und mittlere Pfad-Verzögerung.
#[derive(Debug, Clone)]
pub struct PtpServo {
    /// Glättungsfaktor (0..=1). 1.0 = keine Glättung (Rohwert).
    alpha: f64,
    offset_from_master_ns: f64,
    mean_path_delay_ns: f64,
    have_offset: bool,
    have_delay: bool,
    // Letztes Sync-Paar (Master-Sendezeit t1, lokale Empfangszeit t2).
    last_t1: i128,
    last_t2: i128,
    have_sync: bool,
}

impl PtpServo {
    /// Neuer Servo mit Glättungsfaktor `alpha` (z. B. 0.1). 1.0 = ungeglättet.
    pub fn new(alpha: f64) -> Self {
        Self {
            alpha: alpha.clamp(0.0, 1.0),
            offset_from_master_ns: 0.0,
            mean_path_delay_ns: 0.0,
            have_offset: false,
            have_delay: false,
            last_t1: 0,
            last_t2: 0,
            have_sync: false,
        }
    }

    fn smooth(prev: f64, raw: f64, alpha: f64, have: bool) -> f64 {
        if have {
            prev + alpha * (raw - prev)
        } else {
            raw
        }
    }

    /// Verarbeitet ein Sync/Follow_Up-Paar: `t1` = Master-Sendezeit
    /// (preciseOriginTimestamp), `t2` = lokale Empfangszeit — beide in ns.
    /// Offset = (t2 − t1) − Pfad-Verzögerung.
    pub fn on_sync(&mut self, t1_master_ns: i128, t2_local_ns: i128) {
        self.last_t1 = t1_master_ns;
        self.last_t2 = t2_local_ns;
        self.have_sync = true;
        let raw = (t2_local_ns - t1_master_ns) as f64 - self.mean_path_delay_ns;
        self.offset_from_master_ns = Self::smooth(
            self.offset_from_master_ns,
            raw,
            self.alpha,
            self.have_offset,
        );
        self.have_offset = true;
    }

    /// Verarbeitet ein Delay-Req/Resp-Paar: `t3` = lokale Sendezeit,
    /// `t4` = Master-Empfangszeit — beide in ns. Aktualisiert die
    /// Pfad-Verzögerung = ((t2−t1) + (t4−t3)) / 2 und rechnet den Offset nach.
    pub fn on_delay(&mut self, t3_local_ns: i128, t4_master_ns: i128) {
        if !self.have_sync {
            return; // Ohne Sync-Referenz keine Verzögerung berechenbar.
        }
        let sync_diff = (self.last_t2 - self.last_t1) as f64;
        let delay_diff = (t4_master_ns - t3_local_ns) as f64;
        let raw_delay = (sync_diff + delay_diff) / 2.0;
        self.mean_path_delay_ns = Self::smooth(
            self.mean_path_delay_ns,
            raw_delay,
            self.alpha,
            self.have_delay,
        );
        self.have_delay = true;
        // Offset mit aktualisierter Pfad-Verzögerung neu bestimmen.
        let raw_offset = sync_diff - self.mean_path_delay_ns;
        self.offset_from_master_ns = Self::smooth(
            self.offset_from_master_ns,
            raw_offset,
            self.alpha,
            self.have_offset,
        );
        self.have_offset = true;
    }

    /// Offset Slave − Master in ns (positiv: lokale Uhr geht vor).
    pub fn offset_from_master_ns(&self) -> i64 {
        self.offset_from_master_ns.round() as i64
    }

    /// Mittlere Pfad-Verzögerung in ns.
    pub fn mean_path_delay_ns(&self) -> i64 {
        self.mean_path_delay_ns.round() as i64
    }

    /// Ob schon ein Offset bestimmt wurde.
    pub fn is_locked(&self) -> bool {
        self.have_offset
    }

    /// Rechnet einen lokalen Zeitpunkt in Master-Zeit um.
    pub fn master_from_local_ns(&self, local_ns: i128) -> i128 {
        local_ns - self.offset_from_master_ns.round() as i128
    }
}

/// Eine [`TimeSource`], die eine lokale Uhr um den PTP-Offset korrigiert und so
/// **Master-/Netz-Zeit** liefert. Der Offset wird geteilt (`Arc<AtomicI64>`) und
/// vom Netz-Client via [`PtpTimeSource::offset_handle`] live nachgeführt.
pub struct PtpTimeSource<C: TimeSource> {
    local: C,
    offset_from_master_ns: Arc<AtomicI64>,
}

impl<C: TimeSource> PtpTimeSource<C> {
    pub fn new(local: C) -> Self {
        Self {
            local,
            offset_from_master_ns: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Geteilter Offset-Handle: Der Netz-/Servo-Teil schreibt hier den aktuellen
    /// `offset_from_master_ns` hinein (Slave − Master).
    pub fn offset_handle(&self) -> Arc<AtomicI64> {
        self.offset_from_master_ns.clone()
    }
}

impl<C: TimeSource> TimeSource for PtpTimeSource<C> {
    fn now_nanos(&self) -> u64 {
        let local = self.local.now_nanos() as i128;
        let offset = self.offset_from_master_ns.load(Ordering::Relaxed) as i128;
        (local - offset).max(0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedTimeSource;

    #[test]
    fn offset_without_delay_is_sync_difference() {
        let mut servo = PtpServo::new(1.0); // ungeglättet
        servo.on_sync(1000, 1100); // t2 - t1 = 100, path_delay 0
        assert_eq!(servo.offset_from_master_ns(), 100);
        assert_eq!(servo.mean_path_delay_ns(), 0);
    }

    #[test]
    fn full_offset_and_delay() {
        let mut servo = PtpServo::new(1.0);
        servo.on_sync(1000, 1100); // sync_diff = 100
        servo.on_delay(2000, 2050); // delay_diff = 50
                                    // meanPathDelay = (100 + 50) / 2 = 75
                                    // offset = 100 - 75 = 25
        assert_eq!(servo.mean_path_delay_ns(), 75);
        assert_eq!(servo.offset_from_master_ns(), 25);
    }

    #[test]
    fn master_from_local_applies_offset() {
        let mut servo = PtpServo::new(1.0);
        servo.on_sync(1000, 1100); // offset 100 (Slave geht 100 ns vor)
        assert_eq!(servo.master_from_local_ns(5000), 4900);
    }

    #[test]
    fn delay_before_sync_is_ignored() {
        let mut servo = PtpServo::new(1.0);
        servo.on_delay(2000, 2050); // ohne Sync → kein Effekt
        assert!(!servo.is_locked());
        assert_eq!(servo.mean_path_delay_ns(), 0);
    }

    #[test]
    fn ptp_timesource_corrects_local_by_offset() {
        // Lokale Uhr steht auf 1_000_000_000 ns; Offset (Slave-Master) = +200.
        let ts = PtpTimeSource::new(FixedTimeSource(1_000_000_000));
        assert_eq!(ts.now_nanos(), 1_000_000_000); // ohne Offset
        ts.offset_handle().store(200, Ordering::Relaxed);
        assert_eq!(ts.now_nanos(), 1_000_000_000 - 200);
    }

    #[test]
    fn smoothing_moves_partway() {
        let mut servo = PtpServo::new(0.5);
        servo.on_sync(0, 100); // erster Wert: raw 100 (kein have_offset) → 100
        assert_eq!(servo.offset_from_master_ns(), 100);
        servo.on_sync(0, 200); // raw 200, geglättet: 100 + 0.5*(200-100) = 150
        assert_eq!(servo.offset_from_master_ns(), 150);
    }
}
