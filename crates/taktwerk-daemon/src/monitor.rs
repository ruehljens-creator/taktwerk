//! Geräte- und Traffic-Monitor (application-level, **kein Sniffer**).
//!
//! Aggregiert ausschließlich das, was der Knoten ohnehin sieht:
//! **SAP**- und **PTP**-Control-Traffic (vollständig, weil der Knoten beiden
//! Multicast-Gruppen beitritt) sowie **RTP** der aktiv abonnierten Streams.
//! Pro Absender-IP entsteht ein „Gerät" (mit bestem bekannten Namen), dazu
//! Pakete/Bytes je Protokoll — kumulativ und als 1-s-Rate. Keine Rohpaket-
//! Erfassung, keine Sonderrechte.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// Beobachtetes Protokoll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proto {
    Sap,
    Ptp,
    Rtp,
}

impl Proto {
    fn as_str(self) -> &'static str {
        match self {
            Proto::Sap => "sap",
            Proto::Ptp => "ptp",
            Proto::Rtp => "rtp",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Counter {
    packets: u64,
    bytes: u64,
}

/// Zähler je Protokoll: kumulativ (`total`), 1-s-Rate (`rate`) + interner
/// Schnappschuss zur Ratenbildung.
#[derive(Debug, Clone, Copy, Default)]
struct Stat {
    total: Counter,
    rate: Counter,
    snap: Counter,
}

impl Stat {
    fn add(&mut self, bytes: usize) {
        self.total.packets += 1;
        self.total.bytes += bytes as u64;
    }
    /// Bildet die Rate seit dem letzten Tick (1 s Abstand aufrufen).
    fn tick(&mut self) {
        self.rate = Counter {
            packets: self.total.packets - self.snap.packets,
            bytes: self.total.bytes - self.snap.bytes,
        };
        self.snap = self.total;
    }
    fn json(&self) -> Value {
        json!({
            "packets": self.total.packets,
            "bytes": self.total.bytes,
            "pps": self.rate.packets,
            "bps": self.rate.bytes,
        })
    }
}

#[derive(Debug, Default)]
struct Device {
    name: Option<String>,
    first_seen: u64,
    last_seen: u64,
    protos: BTreeMap<Proto, Stat>,
}

/// Der geteilte Monitor (hinter `Mutex` im App-State).
#[derive(Debug, Default)]
pub struct TrafficMonitor {
    devices: BTreeMap<Ipv4Addr, Device>,
    totals: BTreeMap<Proto, Stat>,
}

impl TrafficMonitor {
    /// Verbucht ein beobachtetes Datagramm. `name` ist ein optionaler Hinweis
    /// (SAP-Session-Name oder PTP-Clock-ID); SAP-Namen haben Vorrang.
    pub fn record(&mut self, proto: Proto, ip: Ipv4Addr, bytes: usize, name: Option<String>) {
        let now = now_unix();
        let dev = self.devices.entry(ip).or_default();
        if dev.first_seen == 0 {
            dev.first_seen = now;
        }
        dev.last_seen = now;
        // Namenswahl: übernehmen, wenn noch keiner da ist, oder wenn ein
        // menschlicher SAP-Name einen technischen PTP-Namen ersetzt.
        if let Some(n) = name {
            let replace = match &dev.name {
                None => true,
                Some(cur) => cur.starts_with("PTP ") && !n.starts_with("PTP "),
            };
            if replace {
                dev.name = Some(n);
            }
        }
        dev.protos.entry(proto).or_default().add(bytes);
        self.totals.entry(proto).or_default().add(bytes);
    }

    /// Registriert ein Gerät **ohne** Traffic (z. B. per mDNS/RAVENNA entdeckt).
    /// Setzt/verbessert nur IP-Eintrag, Name und `last_seen`.
    pub fn note_device(&mut self, ip: Ipv4Addr, name: Option<String>) {
        let now = now_unix();
        let dev = self.devices.entry(ip).or_default();
        if dev.first_seen == 0 {
            dev.first_seen = now;
        }
        dev.last_seen = now;
        if let Some(n) = name {
            let replace = match &dev.name {
                None => true,
                Some(cur) => cur.starts_with("PTP ") && !n.starts_with("PTP "),
            };
            if replace {
                dev.name = Some(n);
            }
        }
    }

    /// Aktualisiert alle 1-s-Raten (im Sekundentakt aufrufen).
    pub fn tick(&mut self) {
        for dev in self.devices.values_mut() {
            for s in dev.protos.values_mut() {
                s.tick();
            }
        }
        for s in self.totals.values_mut() {
            s.tick();
        }
    }

    /// Geräteliste als JSON (IP, Name, Protokolle, Traffic je Protokoll, Zeiten).
    pub fn devices_json(&self) -> Value {
        let now = now_unix();
        let list: Vec<Value> = self
            .devices
            .iter()
            .map(|(ip, dev)| {
                let protos: BTreeMap<&str, Value> = dev
                    .protos
                    .iter()
                    .map(|(p, s)| (p.as_str(), s.json()))
                    .collect();
                let (packets, bytes, pps, bps) = dev.protos.values().fold((0, 0, 0, 0), |a, s| {
                    (
                        a.0 + s.total.packets,
                        a.1 + s.total.bytes,
                        a.2 + s.rate.packets,
                        a.3 + s.rate.bytes,
                    )
                });
                json!({
                    "ip": ip.to_string(),
                    "name": dev.name,
                    "protocols": dev.protos.keys().map(|p| p.as_str()).collect::<Vec<_>>(),
                    "packets": packets,
                    "bytes": bytes,
                    "pps": pps,
                    "bps": bps,
                    "by_proto": protos,
                    "first_seen": dev.first_seen,
                    "last_seen": dev.last_seen,
                    "age_s": now.saturating_sub(dev.last_seen),
                })
            })
            .collect();
        json!(list)
    }

    /// Gesamter Traffic als JSON: je Protokoll + Gesamtsumme.
    pub fn traffic_json(&self) -> Value {
        let by_proto: BTreeMap<&str, Value> = self
            .totals
            .iter()
            .map(|(p, s)| (p.as_str(), s.json()))
            .collect();
        let (packets, bytes, pps, bps) = self.totals.values().fold((0, 0, 0, 0), |a, s| {
            (
                a.0 + s.total.packets,
                a.1 + s.total.bytes,
                a.2 + s.rate.packets,
                a.3 + s.rate.bytes,
            )
        });
        json!({
            "device_count": self.devices.len(),
            "by_proto": by_proto,
            "total": { "packets": packets, "bytes": bytes, "pps": pps, "bps": bps },
        })
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_totals() {
        let mut m = TrafficMonitor::default();
        let ip = Ipv4Addr::new(192, 168, 1, 5);
        m.record(Proto::Sap, ip, 100, Some("Foo".into()));
        m.record(Proto::Sap, ip, 100, None);
        m.record(Proto::Rtp, ip, 300, None);
        let t = m.traffic_json();
        assert_eq!(t["total"]["packets"], 3);
        assert_eq!(t["total"]["bytes"], 500);
        assert_eq!(t["by_proto"]["sap"]["packets"], 2);
        assert_eq!(t["device_count"], 1);

        let d = m.devices_json();
        assert_eq!(d[0]["ip"], "192.168.1.5");
        assert_eq!(d[0]["name"], "Foo");
        assert_eq!(d[0]["packets"], 3);
    }

    #[test]
    fn sap_name_replaces_ptp_name() {
        let mut m = TrafficMonitor::default();
        let ip = Ipv4Addr::new(10, 0, 0, 1);
        m.record(Proto::Ptp, ip, 64, Some("PTP 90:1b:0e".into()));
        assert_eq!(m.devices_json()[0]["name"], "PTP 90:1b:0e");
        m.record(Proto::Sap, ip, 200, Some("Kamera 3".into()));
        assert_eq!(m.devices_json()[0]["name"], "Kamera 3");
    }

    #[test]
    fn rate_is_delta_since_last_tick() {
        let mut m = TrafficMonitor::default();
        let ip = Ipv4Addr::new(10, 0, 0, 2);
        m.record(Proto::Rtp, ip, 300, None);
        m.record(Proto::Rtp, ip, 300, None);
        m.tick(); // Rate = 2 Pakete / 600 Bytes
        let t = m.traffic_json();
        assert_eq!(t["by_proto"]["rtp"]["pps"], 2);
        assert_eq!(t["by_proto"]["rtp"]["bps"], 600);
        m.tick(); // seither nichts → Rate 0
        assert_eq!(m.traffic_json()["by_proto"]["rtp"]["pps"], 0);
    }
}
