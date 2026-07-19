//! Jitter-/Reorder-Puffer für den Empfang (§ Robustheit).
//!
//! RTP-Pakete können über UDP **umsortiert**, **dupliziert** oder **verloren**
//! ankommen. Diese OS-neutrale, testbare Stufe sitzt zwischen dem Empfänger und
//! dem Audio-Backend: sie gibt Pakete **in Sequenz-Reihenfolge** aus, hält eine
//! kleine Reorder-Tiefe vor, verwirft Duplikate/zu späte Pakete und füllt echte
//! Lücken mit **Stille** (Concealment), damit die Wiedergabe-Zeitachse nie
//! springt. Die Zusatzlatenz ist hart auf `depth` Pakete begrenzt.
//!
//! Sequenznummern sind `u16` und laufen über; Vergleiche nutzen Serial-Number-
//! Arithmetik (RFC 1982): `seq - next` als `i16` interpretiert.

use std::collections::HashMap;

/// Laufende Zähler des Jitter-Puffers (für Status/Logs).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JitterStats {
    /// In-Sequenz ausgegebene (echte) Pakete.
    pub emitted: u64,
    /// Für verlorene Pakete eingefügte Stille-Blöcke.
    pub concealed: u64,
    /// Verworfene Duplikate.
    pub duplicate: u64,
    /// Zu spät (älter als der nächste erwartete) → verworfen.
    pub late: u64,
    /// Pakete, die „voraus" ankamen (Lücke davor) — Maß für Umsortierung.
    pub out_of_order: u64,
    /// Harte Neusynchronisationen nach großem Sequenz-Sprung (Sender-Neustart).
    pub resyncs: u64,
}

/// Ab dieser Lücke (in Paketen) wird nicht mehr kaschiert, sondern hart neu
/// aufgesetzt — ein Sender-Neustart mit zufälliger Sequenznummer würde sonst
/// zehntausende Stille-Blöcke in einem Rutsch erzeugen. 64 Pakete ≈ 64 ms
/// (Level A) Verlust sind ohnehin nicht sinnvoll kaschierbar.
const RESYNC_GAP: i16 = 64;

/// Reorder-/Concealment-Puffer für einen Stream mit fester Blockgröße.
pub struct JitterBuffer {
    /// Nächste auszugebende Sequenznummer (None bis zum ersten Paket).
    next_seq: Option<u16>,
    /// Zwischengehaltene, noch nicht ausgebbare Pakete (seq → Samples).
    pending: HashMap<u16, Vec<i32>>,
    /// Länge eines Stille-Blocks (Frames × Kanäle) für Concealment.
    silence_len: usize,
    /// Maximale Reorder-Tiefe in Paketen (begrenzt Latenz und Speicher).
    depth: usize,
    stats: JitterStats,
}

impl JitterBuffer {
    /// `silence_len` = Samples pro Paket (Frames × Kanäle). `depth` = Reorder-
    /// Tiefe in Paketen (z. B. 4 → ≤ 4·ptime Zusatzlatenz).
    pub fn new(silence_len: usize, depth: usize) -> Self {
        Self {
            next_seq: None,
            pending: HashMap::new(),
            silence_len,
            depth: depth.max(1),
            stats: JitterStats::default(),
        }
    }

    /// Aktuelle Statistik.
    pub fn stats(&self) -> JitterStats {
        self.stats
    }

    /// Nimmt ein Paket (`seq`, `samples`) auf und hängt alle jetzt ausgebbaren
    /// Blöcke **in Reihenfolge** an `out` an (echte Pakete oder Stille-Lücken).
    pub fn push(&mut self, seq: u16, samples: Vec<i32>, out: &mut Vec<Vec<i32>>) {
        let next = match self.next_seq {
            None => {
                self.next_seq = Some(seq);
                seq
            }
            Some(n) => n,
        };

        let mut diff = seq.wrapping_sub(next) as i16;
        if diff < 0 {
            self.stats.late += 1; // älter als erwartet → verwerfen
            return;
        }
        if diff > RESYNC_GAP {
            // Sender-Neustart / Riesen-Lücke: gehaltene Pakete geordnet ausgeben
            // (Lücken dazwischen als Stille), dann hart auf `seq` neu aufsetzen —
            // statt die gesamte Lücke Block für Block zu kaschieren.
            self.flush(out);
            self.next_seq = Some(seq);
            self.stats.resyncs += 1;
            diff = 0; // ab hier ist `seq` das erwartete nächste Paket
        }
        if self.pending.contains_key(&seq) {
            self.stats.duplicate += 1;
            return;
        }
        if diff > 0 {
            self.stats.out_of_order += 1; // kam vor einem noch fehlenden Paket
        }
        self.pending.insert(seq, samples);
        self.drain(out);
    }

    /// Gibt aus, solange das nächste Paket vorliegt — oder erzwingt Concealment,
    /// wenn die Reorder-Tiefe überschritten ist (fehlendes Paket aufgeben).
    fn drain(&mut self, out: &mut Vec<Vec<i32>>) {
        loop {
            let next = self.next_seq.expect("next_seq nach erstem push gesetzt");
            if let Some(samples) = self.pending.remove(&next) {
                out.push(samples);
                self.stats.emitted += 1;
                self.next_seq = Some(next.wrapping_add(1));
            } else if self.pending.len() > self.depth {
                // Lücke zu lang: fehlendes Paket mit Stille überbrücken.
                out.push(vec![0i32; self.silence_len]);
                self.stats.concealed += 1;
                self.next_seq = Some(next.wrapping_add(1));
            } else {
                break; // auf das nächste Paket warten
            }
        }
    }

    /// Gibt alle noch gehaltenen Pakete geordnet aus (beim Stopp), fehlende
    /// dazwischen als Stille. Danach ist der Puffer leer.
    pub fn flush(&mut self, out: &mut Vec<Vec<i32>>) {
        while !self.pending.is_empty() {
            let next = self.next_seq.expect("next_seq gesetzt");
            if let Some(samples) = self.pending.remove(&next) {
                out.push(samples);
                self.stats.emitted += 1;
            } else {
                out.push(vec![0i32; self.silence_len]);
                self.stats.concealed += 1;
            }
            self.next_seq = Some(next.wrapping_add(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(v: i32, n: usize) -> Vec<i32> {
        vec![v; n]
    }

    #[test]
    fn in_order_passes_through() {
        let mut jb = JitterBuffer::new(4, 4);
        let mut out = Vec::new();
        for seq in 0..5u16 {
            jb.push(seq, pkt(seq as i32 + 1, 4), &mut out);
        }
        assert_eq!(out.len(), 5);
        assert_eq!(out[0][0], 1);
        assert_eq!(out[4][0], 5);
        assert_eq!(jb.stats().emitted, 5);
        assert_eq!(jb.stats().concealed, 0);
    }

    #[test]
    fn reorders_within_depth() {
        let mut jb = JitterBuffer::new(4, 4);
        let mut out = Vec::new();
        // 0, dann 2, dann 1 → am Ende Reihenfolge 0,1,2
        jb.push(0, pkt(10, 4), &mut out);
        jb.push(2, pkt(12, 4), &mut out); // hält (wartet auf 1)
        assert_eq!(out.len(), 1, "nur Paket 0 ausgegeben, 2 wartet");
        jb.push(1, pkt(11, 4), &mut out); // füllt Lücke → 1 und 2 raus
        assert_eq!(out.len(), 3);
        assert_eq!(out[1][0], 11);
        assert_eq!(out[2][0], 12);
        assert_eq!(jb.stats().concealed, 0);
        assert_eq!(jb.stats().out_of_order, 1);
    }

    #[test]
    fn conceals_lost_packet_beyond_depth() {
        let mut jb = JitterBuffer::new(2, 2); // Tiefe 2
        let mut out = Vec::new();
        jb.push(0, pkt(1, 2), &mut out); // raus
                                         // 1 fehlt dauerhaft; 2,3,4 kommen → sobald pending > depth, wird 1 kaschiert
        jb.push(2, pkt(3, 2), &mut out);
        jb.push(3, pkt(4, 2), &mut out);
        jb.push(4, pkt(5, 2), &mut out); // pending {2,3,4} > 2 → 1 als Stille, dann 2,3,4
        let vals: Vec<i32> = out.iter().map(|b| b[0]).collect();
        assert_eq!(vals, vec![1, 0, 3, 4, 5]); // 0 = kaschierte Stille für seq 1
        assert_eq!(jb.stats().concealed, 1);
        assert_eq!(jb.stats().emitted, 4);
    }

    #[test]
    fn drops_duplicates_and_late() {
        let mut jb = JitterBuffer::new(2, 4);
        let mut out = Vec::new();
        jb.push(5, pkt(1, 2), &mut out); // erstes Paket → Start bei 5
        jb.push(5, pkt(9, 2), &mut out); // Duplikat (bereits ausgegeben → late)
        jb.push(6, pkt(2, 2), &mut out);
        jb.push(4, pkt(9, 2), &mut out); // älter als next → late
        assert_eq!(jb.stats().emitted, 2);
        assert_eq!(jb.stats().late + jb.stats().duplicate, 2);
        let vals: Vec<i32> = out.iter().map(|b| b[0]).collect();
        assert_eq!(vals, vec![1, 2]);
    }

    #[test]
    fn huge_gap_resyncs_instead_of_flooding_silence() {
        let mut jb = JitterBuffer::new(2, 4);
        let mut out = Vec::new();
        jb.push(0, pkt(1, 2), &mut out);
        // Sender-Neustart: Sprung um 30000 — darf KEINE 30000 Stille-Blöcke
        // erzeugen, sondern hart neu aufsetzen.
        jb.push(30000, pkt(2, 2), &mut out);
        jb.push(30001, pkt(3, 2), &mut out);
        let vals: Vec<i32> = out.iter().map(|b| b[0]).collect();
        assert_eq!(vals, vec![1, 2, 3], "keine Stille-Flut, nahtloser Resync");
        assert_eq!(jb.stats().resyncs, 1);
        assert_eq!(jb.stats().concealed, 0);
        assert_eq!(jb.stats().out_of_order, 0, "Resync zählt nicht als Reorder");
    }

    #[test]
    fn sequence_wraparound_is_handled() {
        let mut jb = JitterBuffer::new(1, 4);
        let mut out = Vec::new();
        jb.push(65534, pkt(1, 1), &mut out);
        jb.push(65535, pkt(2, 1), &mut out);
        jb.push(0, pkt(3, 1), &mut out); // Überlauf
        jb.push(1, pkt(4, 1), &mut out);
        let vals: Vec<i32> = out.iter().map(|b| b[0]).collect();
        assert_eq!(vals, vec![1, 2, 3, 4]);
        assert_eq!(jb.stats().concealed, 0);
    }
}
