//! Optionale **TOML-Konfigdatei** als Alternative/Ergänzung zu den
//! Umgebungsvariablen. **Env hat immer Vorrang** — die Datei liefert nur
//! Vorbelegungen für nicht gesetzte Variablen.
//!
//! Fundort: `TAKTWERK_CONFIG` (expliziter Pfad), sonst `./taktwerk.toml` oder
//! `/etc/taktwerk/taktwerk.toml`. Fehlt die Datei, passiert nichts (reiner
//! Env-/Default-Betrieb wie bisher).
//!
//! Beispiel `taktwerk.toml`:
//! ```toml
//! name = "studio-node"
//! iface = "10.0.0.20"
//! http = "0.0.0.0:7788"
//! channels = 8
//! ptp_slave = true
//! audio = "cpal"
//! audio_in = "Loopback"
//! log = "info"
//! ```

use serde::Deserialize;

/// Konfig-Felder (alle optional). Namen entsprechen den `TAKTWERK_*`-Variablen.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub name: Option<String>,
    pub iface: Option<String>,
    pub http: Option<String>,
    pub nmos: Option<String>,
    pub rtsp: Option<String>,
    pub channels: Option<u8>,
    pub ptp_slave: Option<bool>,
    pub ptp_master: Option<bool>,
    pub audio: Option<String>,
    pub audio_in: Option<String>,
    pub audio_out: Option<String>,
    pub log: Option<String>,
    pub log_file: Option<String>,
}

impl FileConfig {
    /// Lädt die Konfig aus dem ersten gefundenen Pfad. Parsefehler sind nicht
    /// fatal (Warnung auf stderr, weiter mit Env/Defaults). Logging läuft zu
    /// diesem Zeitpunkt noch nicht — daher `eprintln!` statt `tracing`.
    pub fn load() -> Self {
        let Some(path) = Self::resolve_path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<FileConfig>(&text) {
                Ok(cfg) => {
                    eprintln!("taktwerkd: Konfig geladen aus {path}");
                    cfg
                }
                Err(e) => {
                    eprintln!("taktwerkd: Konfig {path} nicht parsebar ({e}) — ignoriert");
                    Self::default()
                }
            },
            Err(e) => {
                eprintln!("taktwerkd: Konfig {path} nicht lesbar ({e}) — ignoriert");
                Self::default()
            }
        }
    }

    /// Sucht den Konfig-Pfad: `TAKTWERK_CONFIG`, sonst bekannte Standardorte.
    fn resolve_path() -> Option<String> {
        if let Ok(p) = std::env::var("TAKTWERK_CONFIG") {
            return Some(p);
        }
        ["taktwerk.toml", "/etc/taktwerk/taktwerk.toml"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .map(String::from)
    }

    /// Schreibt gesetzte Felder in die passenden `TAKTWERK_*`-Env-Variablen —
    /// aber **nur, wenn diese nicht schon gesetzt sind** (Env behält Vorrang).
    /// Danach liest der restliche Daemon unverändert aus der Umgebung.
    pub fn apply_to_env(&self) {
        set_if_absent("TAKTWERK_NAME", self.name.as_deref());
        set_if_absent("TAKTWERK_IFACE", self.iface.as_deref());
        set_if_absent("TAKTWERK_HTTP", self.http.as_deref());
        set_if_absent("TAKTWERK_NMOS", self.nmos.as_deref());
        set_if_absent("TAKTWERK_RTSP", self.rtsp.as_deref());
        set_if_absent(
            "TAKTWERK_CH",
            self.channels.map(|c| c.to_string()).as_deref(),
        );
        set_if_absent("TAKTWERK_PTP_SLAVE", self.ptp_slave.map(bool01).as_deref());
        set_if_absent(
            "TAKTWERK_PTP_MASTER",
            self.ptp_master.map(bool01).as_deref(),
        );
        set_if_absent("TAKTWERK_AUDIO", self.audio.as_deref());
        set_if_absent("TAKTWERK_AUDIO_IN", self.audio_in.as_deref());
        set_if_absent("TAKTWERK_AUDIO_OUT", self.audio_out.as_deref());
        set_if_absent("TAKTWERK_LOG", self.log.as_deref());
        set_if_absent("TAKTWERK_LOG_FILE", self.log_file.as_deref());
    }
}

fn bool01(b: bool) -> String {
    if b {
        "1".into()
    } else {
        "0".into()
    }
}

/// Setzt `key` auf `val`, falls `val` vorhanden **und** `key` nicht schon gesetzt.
fn set_if_absent(key: &str, val: Option<&str>) {
    if let Some(v) = val {
        if std::env::var_os(key).is_none() {
            std::env::set_var(key, v);
        }
    }
}
