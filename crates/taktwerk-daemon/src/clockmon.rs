//! Referenzuhr-Monitor für das Clock-Panel im UI (Machbarkeitsstudie):
//!
//! - **Drift-Schätzer:** tastet den PTP-Slave-Offset sekündlich ab und schätzt
//!   per linearer Regression die Steigung (ns/s = ppb) — also wie schnell die
//!   lokale Uhr gegen den Grandmaster driftet. Ausgabe in ppm, % und „Hz bei
//!   48 kHz" (Drift der Media-Clock).
//! - **GNSS-Status via gpsd:** verbindet sich mit einem lokalen `gpsd`
//!   (JSON-Protokoll, Port 2947) und übernimmt Fix-Status (TPV) und
//!   Satellitenliste mit Signalstärken (SKY). Ohne gpsd/Hardware bleibt der
//!   Status sauber „nicht verbunden" — sobald das GNSS-Modul am Host hängt,
//!   füllt sich das Panel ohne Codeänderung.
//! - `GET /clock` liefert beides plus die Serverzeit (Unix-ms; das UI formatiert).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info, warn};

use crate::state::AppState;

/// Ein Satellit aus der gpsd-SKY-Meldung.
#[derive(Debug, Default, Clone, Serialize)]
pub struct GnssSat {
    /// Satelliten-ID (PRN).
    pub prn: u16,
    /// Signal-Rausch-Verhältnis in dB-Hz (0 = nicht gemessen).
    pub snr: f64,
    /// Wird dieser Satellit für die Lösung benutzt?
    pub used: bool,
}

/// GNSS-Zustand (von gpsd gespeist).
#[derive(Debug, Default, Clone, Serialize)]
pub struct GnssStatus {
    /// gpsd erreichbar und Daten fließen.
    pub connected: bool,
    /// Fix-Modus: 0/1 = kein Fix, 2 = 2D, 3 = 3D.
    pub mode: u8,
    /// GNSS-Zeit (ISO-String aus TPV), falls Fix.
    pub time: Option<String>,
    pub sats_visible: usize,
    pub sats_used: usize,
    /// Satelliten sortiert nach SNR absteigend (fürs Balkendiagramm).
    pub sats: Vec<GnssSat>,
}

/// Geschätzte Drift der lokalen Uhr gegenüber der PTP-Referenz.
#[derive(Debug, Default, Clone, Serialize)]
pub struct DriftStatus {
    /// true, wenn genug Messpunkte für eine belastbare Schätzung da sind.
    pub valid: bool,
    /// Drift in ppm (positiv = lokale Uhr läuft zu schnell).
    pub ppm: f64,
    /// Entsprechende Abweichung der 48-kHz-Media-Clock in Hz.
    pub hz_at_48k: f64,
    /// Drift in Prozent.
    pub percent: f64,
    /// Länge des Beobachtungsfensters in Sekunden.
    pub window_s: u64,
    pub samples: usize,
}

/// Kleinste-Quadrate-Steigung über (t_s, offset_ns)-Punkte → ns/s (= ppb).
/// None bei < 2 Punkten oder entarteter Zeitbasis.
fn slope_ns_per_s(points: &[(f64, f64)]) -> Option<f64> {
    let n = points.len() as f64;
    if points.len() < 2 {
        return None;
    }
    let sum_t: f64 = points.iter().map(|(t, _)| t).sum();
    let sum_y: f64 = points.iter().map(|(_, y)| y).sum();
    let mean_t = sum_t / n;
    let mean_y = sum_y / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (t, y) in points {
        num += (t - mean_t) * (y - mean_y);
        den += (t - mean_t) * (t - mean_t);
    }
    if den.abs() < f64::EPSILON {
        return None;
    }
    Some(num / den)
}

fn now_s() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Tastet den PTP-Offset ab und pflegt die Drift-Schätzung (nur sinnvoll im
/// Slave-Modus; sonst bleibt `valid = false`).
pub async fn drift_task(state: AppState) {
    const WINDOW: usize = 60; // Sekunden Beobachtungsfenster
    const MIN_SAMPLES: usize = 10;
    let mut ring: VecDeque<(f64, f64)> = VecDeque::with_capacity(WINDOW);
    let mut tick = interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let (synced, offset_ns) = {
            let p = state.ptp.lock().unwrap();
            (p.synced, p.offset_ns)
        };
        if !(state.node.ptp_slave && synced) {
            ring.clear();
            let mut d = state.drift.lock().unwrap();
            *d = DriftStatus::default();
            continue;
        }
        ring.push_back((now_s(), offset_ns as f64));
        while ring.len() > WINDOW {
            ring.pop_front();
        }
        if ring.len() < MIN_SAMPLES {
            continue;
        }
        let pts: Vec<(f64, f64)> = ring.iter().copied().collect();
        if let Some(ppb) = slope_ns_per_s(&pts) {
            let ppm = ppb / 1000.0;
            let mut d = state.drift.lock().unwrap();
            *d = DriftStatus {
                valid: true,
                ppm,
                hz_at_48k: 48_000.0 * ppb * 1e-9,
                percent: ppb * 1e-9 * 100.0,
                window_s: (pts.last().unwrap().0 - pts[0].0).round() as u64,
                samples: pts.len(),
            };
        }
    }
}

/// gpsd-Adresse aus `TAKTWERK_GPSD` (Default 127.0.0.1:2947; "off" deaktiviert).
fn gpsd_addr() -> Option<String> {
    match std::env::var("TAKTWERK_GPSD") {
        Ok(v) if v.eq_ignore_ascii_case("off") => None,
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => Some("127.0.0.1:2947".into()),
    }
}

/// Verbindet sich (mit Wiederholung) zu gpsd und pflegt [`GnssStatus`].
pub async fn gpsd_task(state: AppState) {
    let Some(addr) = gpsd_addr() else {
        info!("GNSS: gpsd deaktiviert (TAKTWERK_GPSD=off)");
        return;
    };
    let mut announced = false;
    loop {
        match TcpStream::connect(&addr).await {
            Ok(mut sock) => {
                info!(%addr, "GNSS: mit gpsd verbunden");
                announced = false;
                let _ = sock
                    .write_all(b"?WATCH={\"enable\":true,\"json\":true};\n")
                    .await;
                let mut lines = BufReader::new(sock).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        apply_gpsd_msg(&state, &v);
                    }
                }
                warn!("GNSS: gpsd-Verbindung beendet — neuer Versuch in 15 s");
            }
            Err(e) => {
                if !announced {
                    debug!(%addr, "GNSS: gpsd nicht erreichbar ({e}) — Panel zeigt 'kein GNSS'");
                    announced = true;
                }
            }
        }
        state.gnss.lock().unwrap().connected = false;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

/// Übernimmt eine gpsd-JSON-Meldung (TPV = Fix/Zeit, SKY = Satelliten).
fn apply_gpsd_msg(state: &AppState, v: &Value) {
    match v["class"].as_str() {
        Some("TPV") => {
            let mut g = state.gnss.lock().unwrap();
            g.connected = true;
            g.mode = v["mode"].as_u64().unwrap_or(0) as u8;
            g.time = v["time"].as_str().map(String::from);
        }
        Some("SKY") => {
            let mut sats: Vec<GnssSat> = v["satellites"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|s| GnssSat {
                            prn: s["PRN"].as_u64().unwrap_or(0) as u16,
                            snr: s["ss"].as_f64().unwrap_or(0.0),
                            used: s["used"].as_bool().unwrap_or(false),
                        })
                        .collect()
                })
                .unwrap_or_default();
            sats.sort_by(|a, b| {
                b.snr
                    .partial_cmp(&a.snr)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            sats.truncate(24); // UI-Balken begrenzen
            let mut g = state.gnss.lock().unwrap();
            g.connected = true;
            g.sats_visible = v["satellites"].as_array().map(|a| a.len()).unwrap_or(0);
            g.sats_used = sats.iter().filter(|s| s.used).count();
            g.sats = sats;
        }
        _ => {}
    }
}

/// Bekannte Referenzquellen. Auswahl steuert derzeit Anzeige/Reporting; die
/// eigentliche Takt-Disziplinierung folgt mit dem ClockDiscipline-Modul.
/// "aes" (Haustakt via Audio-In) und "wcpps" (Wordclock→1-Hz-Teiler) sind als
/// künftige Quellen schon gelistet, aber bis dahin nicht verfügbar.
const CLOCK_SOURCES: &[&str] = &["auto", "ptp", "gnss", "system", "aes", "wcpps"];

/// Liste der Quellen mit Verfügbarkeit im aktuellen Zustand.
fn sources_json(state: &AppState) -> Value {
    let gnss_ok = state.gnss.lock().unwrap().connected;
    let ptp_ok = state.node.ptp_slave || state.node.ptp_master;
    json!(CLOCK_SOURCES
        .iter()
        .map(|id| {
            let available = match *id {
                "auto" | "system" => true,
                "ptp" => ptp_ok,
                "gnss" => gnss_ok,
                _ => false, // aes/wcpps: Hardware-Wege folgen
            };
            json!({ "id": id, "available": available })
        })
        .collect::<Vec<_>>())
}

/// `GET /clock` — Serverzeit + Rolle/Drift + GNSS + Referenzquellen fürs Panel.
pub async fn clock(State(state): State<AppState>) -> Json<Value> {
    let time_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let role = if state.node.ptp_master {
        "master"
    } else if state.node.ptp_slave {
        "slave"
    } else {
        "system"
    };
    let synced = state.ptp.lock().unwrap().synced;
    let drift = state.drift.lock().unwrap().clone();
    let gnss = state.gnss.lock().unwrap().clone();
    let selected = state.clock_ref.lock().unwrap().clone();
    Json(json!({
        "time_unix_ms": time_unix_ms,
        "role": role,
        "synced": synced,
        "drift": drift,
        "gnss": gnss,
        "sources": sources_json(&state),
        "selected_source": selected,
    }))
}

/// Request für `POST /clock/source`.
#[derive(serde::Deserialize)]
pub struct SourceRequest {
    pub id: String,
}

/// `POST /clock/source` — Referenzquelle wählen (nur bekannte IDs).
pub async fn set_source(
    State(state): State<AppState>,
    Json(req): Json<SourceRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, String)> {
    if !CLOCK_SOURCES.contains(&req.id.as_str()) {
        return Err((
            axum::http::StatusCode::BAD_REQUEST,
            format!("unbekannte Quelle: {}", req.id),
        ));
    }
    *state.clock_ref.lock().unwrap() = req.id.clone();
    info!(source = %req.id, "Referenzquelle gewählt");
    Ok(Json(json!({ "selected_source": req.id })))
}

/// Handle-Typ für den geteilten GNSS-Zustand.
pub type GnssHandle = Arc<Mutex<GnssStatus>>;
/// Handle-Typ für den geteilten Drift-Zustand.
pub type DriftHandle = Arc<Mutex<DriftStatus>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slope_of_linear_series_is_exact() {
        // Offset wächst 500 ns pro Sekunde → Drift +500 ppb = 0,5 ppm.
        let pts: Vec<(f64, f64)> = (0..30).map(|i| (i as f64, 500.0 * i as f64)).collect();
        let s = slope_ns_per_s(&pts).unwrap();
        assert!((s - 500.0).abs() < 1e-9, "Steigung {s} ≠ 500");
    }

    #[test]
    fn slope_is_robust_to_noise_sign() {
        // Fallende Gerade → negative Drift.
        let pts: Vec<(f64, f64)> = (0..30).map(|i| (i as f64, -250.0 * i as f64)).collect();
        let s = slope_ns_per_s(&pts).unwrap();
        assert!((s + 250.0).abs() < 1e-9);
    }

    #[test]
    fn slope_needs_two_points_and_time_spread() {
        assert!(slope_ns_per_s(&[]).is_none());
        assert!(slope_ns_per_s(&[(1.0, 5.0)]).is_none());
        // Gleiche Zeitstempel → entartet.
        assert!(slope_ns_per_s(&[(1.0, 5.0), (1.0, 9.0)]).is_none());
    }

    #[test]
    fn gpsd_sky_message_is_applied() {
        use crate::state::{AppState, NodeInfo};
        use taktwerk_core::StreamProfile;
        let state = AppState::new(NodeInfo {
            name: "t".into(),
            interface: std::net::Ipv4Addr::UNSPECIFIED,
            profile: StreamProfile::level_a(2),
            ptp_slave: false,
            ptp_master: false,
            ptp_domain: 0,
            nmos_host: "0.0.0.0".into(),
            nmos_port: 0,
        });
        let sky: Value = serde_json::json!({
            "class": "SKY",
            "satellites": [
                {"PRN": 5, "ss": 41.0, "used": true},
                {"PRN": 12, "ss": 22.5, "used": false},
                {"PRN": 23, "ss": 38.0, "used": true},
            ]
        });
        apply_gpsd_msg(&state, &sky);
        let g = state.gnss.lock().unwrap();
        assert!(g.connected);
        assert_eq!(g.sats_visible, 3);
        assert_eq!(g.sats_used, 2);
        assert_eq!(g.sats[0].prn, 5, "nach SNR sortiert");

        drop(g);
        let tpv: Value =
            serde_json::json!({"class":"TPV","mode":3,"time":"2026-07-19T12:00:00.000Z"});
        apply_gpsd_msg(&state, &tpv);
        let g = state.gnss.lock().unwrap();
        assert_eq!(g.mode, 3);
        assert_eq!(g.time.as_deref(), Some("2026-07-19T12:00:00.000Z"));
    }
}
