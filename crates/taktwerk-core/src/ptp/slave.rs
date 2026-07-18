//! PTP-Slave-Zustandsmaschine (rein, testbar): ordnet Sync/Follow_Up und
//! Delay_Req/Delay_Resp den Zeitpaaren zu und füttert den [`PtpServo`].
//!
//! Der Netz-Teil (Sockets, Timing) liegt in `taktwerk-net`; hier ist nur die
//! Zuordnungs- und Regel-Logik — damit deterministisch prüfbar.

use std::collections::HashMap;

use super::servo::PtpServo;

/// Verfolgt Sync-/Delay-Austausch und pflegt Offset + Pfad-Verzögerung.
#[derive(Debug, Clone)]
pub struct SlaveState {
    servo: PtpServo,
    /// two-step: seq → t2 (lokale Sync-Empfangszeit), bis Follow_Up kommt.
    pending_sync: HashMap<u16, i128>,
    /// Ausstehendes Delay_Req: (seq, t3 lokale Sendezeit).
    pending_delay: Option<(u16, i128)>,
    synced: bool,
}

impl SlaveState {
    pub fn new(alpha: f64) -> Self {
        Self {
            servo: PtpServo::new(alpha),
            pending_sync: HashMap::new(),
            pending_delay: None,
            synced: false,
        }
    }

    /// Sync empfangen. `two_step` aus dem Sync-Flag: true → auf Follow_Up warten;
    /// false (one-step) → `sync_ts_nanos` ist bereits t1.
    pub fn on_sync(&mut self, seq: u16, sync_ts_nanos: i128, two_step: bool, t2_local: i128) {
        if two_step {
            self.pending_sync.insert(seq, t2_local);
        } else {
            self.apply_sync(sync_ts_nanos, t2_local);
        }
    }

    /// Follow_Up empfangen: liefert das exakte t1 zum zuvor gemerkten t2.
    pub fn on_follow_up(&mut self, seq: u16, t1_master_nanos: i128) {
        if let Some(t2) = self.pending_sync.remove(&seq) {
            self.apply_sync(t1_master_nanos, t2);
        }
    }

    fn apply_sync(&mut self, t1: i128, t2: i128) {
        self.servo.on_sync(t1, t2);
        self.synced = true;
    }

    /// Wir haben ein Delay_Req gesendet: (seq, t3 = lokale Sendezeit).
    pub fn on_delay_req_sent(&mut self, seq: u16, t3_local: i128) {
        self.pending_delay = Some((seq, t3_local));
    }

    /// Delay_Resp empfangen. `requesting_is_us` = requestingPortIdentity == unsere;
    /// `t4` = receiveTimestamp des Masters. Passt seq → Pfad-Verzögerung updaten.
    pub fn on_delay_resp(&mut self, seq: u16, requesting_is_us: bool, t4_master: i128) {
        if !requesting_is_us {
            return;
        }
        if let Some((s, t3)) = self.pending_delay {
            if s == seq {
                self.servo.on_delay(t3, t4_master);
                self.pending_delay = None;
            }
        }
    }

    /// Offset Slave − Master (ns).
    pub fn offset_ns(&self) -> i64 {
        self.servo.offset_from_master_ns()
    }
    /// Mittlere Pfad-Verzögerung (ns).
    pub fn path_delay_ns(&self) -> i64 {
        self.servo.mean_path_delay_ns()
    }
    /// Ob mindestens ein Sync verarbeitet wurde.
    pub fn is_synced(&self) -> bool {
        self.synced
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_step_sync_needs_follow_up() {
        let mut s = SlaveState::new(1.0);
        s.on_sync(1, 0, true, 1100); // t2 = 1100, wartet auf Follow_Up
        assert!(!s.is_synced());
        s.on_follow_up(1, 1000); // t1 = 1000 → offset 100
        assert!(s.is_synced());
        assert_eq!(s.offset_ns(), 100);
    }

    #[test]
    fn one_step_sync_applies_directly() {
        let mut s = SlaveState::new(1.0);
        s.on_sync(1, 1000, false, 1100); // t1=1000, t2=1100 → offset 100
        assert!(s.is_synced());
        assert_eq!(s.offset_ns(), 100);
    }

    #[test]
    fn full_cycle_offset_and_delay() {
        let mut s = SlaveState::new(1.0);
        s.on_sync(1, 1000, false, 1100); // sync_diff = 100
        s.on_delay_req_sent(1, 2000); // t3
        s.on_delay_resp(1, true, 2050); // t4 → delay=(100+50)/2=75, offset=100-75=25
        assert_eq!(s.path_delay_ns(), 75);
        assert_eq!(s.offset_ns(), 25);
    }

    #[test]
    fn delay_resp_for_other_port_ignored() {
        let mut s = SlaveState::new(1.0);
        s.on_sync(1, 1000, false, 1100);
        s.on_delay_req_sent(1, 2000);
        s.on_delay_resp(1, false, 2050); // nicht unsere → ignorieren
        assert_eq!(s.path_delay_ns(), 0);
        assert_eq!(s.offset_ns(), 100); // unverändert
    }

    #[test]
    fn mismatched_seq_ignored() {
        let mut s = SlaveState::new(1.0);
        s.on_sync(1, 1000, false, 1100);
        s.on_delay_req_sent(5, 2000);
        s.on_delay_resp(6, true, 2050); // falsche seq
        assert_eq!(s.path_delay_ns(), 0);
    }
}
