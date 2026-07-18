//! Media-Clock-Abstraktion: die Zeitquelle, aus der RTP-Timestamps entstehen.
//!
//! In AES67 ist der RTP-Timestamp die **Media-Clock in Sample-Ticks**, abgeleitet
//! aus der PTP-Netzzeit. Damit der Rest des Systems nicht weiss, *woher* die Zeit
//! kommt (PTP-Slave, PTP-Master, oder im Headless-Betrieb die Systemuhr), liegt
//! sie hinter dem [`TimeSource`]-Trait — eine der zentralen OS-/Provider-Nähte.
//!
//! - Phase 0 (headless): [`SystemTimeSource`] (Wanduhr). Ausreichend, um Streams
//!   zu erzeugen und die Pipeline zu testen.
//! - Phase 1: eine PTP-gebundene Implementierung (Slave lockt an den Grandmaster)
//!   ersetzt sie hinter demselben Trait — kein Aufrufer aendert sich.

/// Eine Zeitquelle, die eine monoton fortschreitende Referenz in Nanosekunden
/// liefert und daraus Media-Clock-/RTP-Timestamps ableitet.
pub trait TimeSource: Send + Sync {
    /// Aktuelle Zeit der Uhr in Nanosekunden (seit einer quellen-eigenen Epoche).
    fn now_nanos(&self) -> u64;

    /// RTP-Timestamp (Sample-Ticks) fuer „jetzt" bei gegebener Abtastrate.
    /// Wrappt bei 2^32 wie im RTP-Header vorgesehen.
    fn rtp_timestamp(&self, sample_rate: u32) -> u32 {
        let ticks = (self.now_nanos() as u128 * sample_rate as u128) / 1_000_000_000u128;
        (ticks & 0xFFFF_FFFF) as u32
    }
}

/// Zeitquelle auf Basis der Systemuhr (`SystemTime`). Für Headless/Phase 0.
/// **Kein** PTP — die Absolutzeit ist nicht netzsynchron; für lokale Erzeugung
/// und Tests genügt sie.
#[derive(Debug, Clone, Default)]
pub struct SystemTimeSource;

impl TimeSource for SystemTimeSource {
    fn now_nanos(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

/// Feste Zeitquelle für deterministische Tests (setzt `now_nanos` hart).
#[derive(Debug, Clone)]
pub struct FixedTimeSource(pub u64);

impl TimeSource for FixedTimeSource {
    fn now_nanos(&self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_timestamp_from_fixed_time() {
        // Genau 1 Sekunde → sample_rate Ticks.
        let ts = FixedTimeSource(1_000_000_000);
        assert_eq!(ts.rtp_timestamp(48_000), 48_000);
    }

    #[test]
    fn rtp_timestamp_half_second() {
        let ts = FixedTimeSource(500_000_000);
        assert_eq!(ts.rtp_timestamp(48_000), 24_000);
    }

    #[test]
    fn rtp_timestamp_wraps_at_2_pow_32() {
        // Zeitpunkt so gross, dass die Tickzahl > 2^32 ist → muss wrappen.
        let ts = FixedTimeSource(1_000_000_000_000_000); // 1e6 s
        let raw = (1_000_000_000_000_000u128 * 48_000u128) / 1_000_000_000u128;
        assert_eq!(ts.rtp_timestamp(48_000) as u128, raw & 0xFFFF_FFFF);
    }

    #[test]
    fn system_time_source_moves_forward() {
        let ts = SystemTimeSource;
        let a = ts.now_nanos();
        assert!(a > 0);
    }
}
