//! PTP / IEEE 1588 — Datentypen und **BMCA** (Best Master Clock Algorithm).
//!
//! Der Protokoll-Kern ist OS-neutral: Er entscheidet aus zwei Clock-Datensaetzen,
//! welche die bessere Master-Uhr ist. Das *Timestamping* (woher die Zeitstempel
//! kommen — Software auf macOS/Windows, `SO_TIMESTAMPING`/HW auf Linux) sitzt
//! hinter dem `TimeSource`-Trait in `taktwerk-ptp` und beruehrt diese Logik nicht.
//!
//! Zwei PTP-Profile sind Ziel (§7.2): AES67-Media-Profil und SMPTE ST 2059-2 —
//! sie unterscheiden sich in Domain/Intervallen, nicht in der BMCA-Ordnung.

/// EUI-64 Clock-Identity.
pub type ClockIdentity = [u8; 8];

/// Der fuer die BMCA relevante Announce-Datensatz einer Uhr (RFC 1588, 9.3.2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockDataset {
    pub priority1: u8,
    pub clock_class: u8,
    pub clock_accuracy: u8,
    /// `offsetScaledLogVariance` — Stabilitaetsmass.
    pub offset_scaled_log_variance: u16,
    pub priority2: u8,
    pub clock_identity: ClockIdentity,
    /// Anzahl Boundary-Clocks bis zu dieser Uhr (steps removed).
    pub steps_removed: u16,
}

/// Ergebnis eines BMCA-Vergleichs aus Sicht von `a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmcaOrder {
    /// `a` ist die bessere Master-Uhr.
    ABetter,
    /// `b` ist die bessere Master-Uhr.
    BBetter,
    /// Identische Uhr (gleiche clock_identity).
    Same,
}

impl ClockDataset {
    /// Vergleicht zwei Uhren nach der BMCA-Dataset-Comparison. Die Ordnung ist
    /// lexikografisch ueber: priority1 → clockClass → clockAccuracy →
    /// offsetScaledLogVariance → priority2 → clockIdentity (jeweils **kleiner =
    /// besser**). steps_removed ist nur Tiebreaker gleicher Identitaet-Pfade.
    pub fn compare(a: &ClockDataset, b: &ClockDataset) -> BmcaOrder {
        if a.clock_identity == b.clock_identity {
            return BmcaOrder::Same;
        }
        // Kette von "kleiner ist besser"-Feldern.
        let a_key = (
            a.priority1,
            a.clock_class,
            a.clock_accuracy,
            a.offset_scaled_log_variance,
            a.priority2,
        );
        let b_key = (
            b.priority1,
            b.clock_class,
            b.clock_accuracy,
            b.offset_scaled_log_variance,
            b.priority2,
        );
        match a_key.cmp(&b_key) {
            core::cmp::Ordering::Less => BmcaOrder::ABetter,
            core::cmp::Ordering::Greater => BmcaOrder::BBetter,
            core::cmp::Ordering::Equal => {
                // Letzter Tiebreaker: kleinere clockIdentity gewinnt.
                if a.clock_identity < b.clock_identity {
                    BmcaOrder::ABetter
                } else {
                    BmcaOrder::BBetter
                }
            }
        }
    }

    /// Waehlt aus einer Menge fremder Uhren die beste; None, wenn leer.
    pub fn best<'a>(datasets: impl IntoIterator<Item = &'a ClockDataset>) -> Option<&'a ClockDataset> {
        datasets.into_iter().reduce(|best, cur| {
            match ClockDataset::compare(best, cur) {
                BmcaOrder::ABetter | BmcaOrder::Same => best,
                BmcaOrder::BBetter => cur,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ds(priority1: u8, clock_class: u8, id_last: u8) -> ClockDataset {
        ClockDataset {
            priority1,
            clock_class,
            clock_accuracy: 0x21,
            offset_scaled_log_variance: 0,
            priority2: 128,
            clock_identity: [0, 0, 0, 0, 0, 0, 0, id_last],
            steps_removed: 0,
        }
    }

    #[test]
    fn priority1_dominates() {
        let a = ds(10, 248, 1);
        let b = ds(128, 6, 2); // bessere Klasse, aber schlechtere priority1
        assert_eq!(ClockDataset::compare(&a, &b), BmcaOrder::ABetter);
    }

    #[test]
    fn class_breaks_tie_after_priority1() {
        let a = ds(128, 6, 1);
        let b = ds(128, 248, 2);
        assert_eq!(ClockDataset::compare(&a, &b), BmcaOrder::ABetter);
    }

    #[test]
    fn identity_is_final_tiebreaker() {
        let a = ds(128, 248, 1);
        let b = ds(128, 248, 2);
        assert_eq!(ClockDataset::compare(&a, &b), BmcaOrder::ABetter);
    }

    #[test]
    fn same_identity_detected() {
        let a = ds(1, 6, 9);
        let b = ds(200, 248, 9);
        assert_eq!(ClockDataset::compare(&a, &b), BmcaOrder::Same);
    }

    #[test]
    fn best_of_set() {
        let clocks = [ds(128, 248, 3), ds(50, 248, 4), ds(50, 6, 5)];
        let best = ClockDataset::best(&clocks).unwrap();
        assert_eq!(best.clock_class, 6); // priority1=50 & Klasse 6
    }

    /// §5 des Briefs: mit `priority1` gewinnt unsere Box die Wahl bewusst.
    #[test]
    fn local_master_wins_with_low_priority1() {
        let external = ds(128, 6, 1); // guter externer GM
        let local = ds(1, 248, 2); // unsere Box, priority1 hart auf 1
        assert_eq!(ClockDataset::compare(&local, &external), BmcaOrder::ABetter);
    }
}
