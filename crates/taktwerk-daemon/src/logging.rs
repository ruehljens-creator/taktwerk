//! Debug-Log-Einrichtung (tracing).
//!
//! Schreibt strukturierte Logs **gleichzeitig nach stderr und in eine
//! Debug-Datei**. Standardmäßig laufen alle `taktwerk_*`-Crates auf `debug`,
//! Fremd-Crates (tokio/hyper/…) auf `info` — so ist die Datei ein echtes
//! Debug-Log, ohne im Fremd-Rauschen unterzugehen.
//!
//! Konfiguration über Env:
//! - `TAKTWERK_LOG`      — Filter-Direktiven (wie `RUST_LOG`), überschreibt Default.
//!   Beispiele: `debug`, `trace`, `taktwerk_net=trace,info`.
//! - `TAKTWERK_LOG_FILE` — Pfad der Debug-Datei (Default `taktwerk-debug.log`).

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

/// Default-Filter: eigene Crates ausführlich (`debug`), Rest ruhig (`info`).
const DEFAULT_FILTER: &str = "info,\
    taktwerk_core=debug,\
    taktwerk_net=debug,\
    taktwerk_endpoint=debug,\
    taktwerk_router=debug,\
    taktwerk_daemon=debug";

/// Richtet das globale Logging ein. Der zurückgegebene [`WorkerGuard`] muss am
/// Leben bleiben (sonst wird der Datei-Writer geschlossen) — in `main` halten.
pub fn init() -> Option<WorkerGuard> {
    let filter = EnvFilter::try_from_env("TAKTWERK_LOG")
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    let log_file =
        std::env::var("TAKTWERK_LOG_FILE").unwrap_or_else(|_| "taktwerk-debug.log".to_string());

    // Konsolen-Layer (kompakt, mit Zielangabe).
    let stderr_layer = fmt::layer().with_target(true).with_writer(std::io::stderr);

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
    {
        Ok(file) => {
            let (non_blocking, guard) = tracing_appender::non_blocking(file);
            let file_layer = fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_writer(non_blocking);
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .with(file_layer)
                .init();
            tracing::info!(file = %log_file, "Debug-Log aktiv (stderr + Datei)");
            Some(guard)
        }
        Err(e) => {
            // Ohne beschreibbare Datei nur Konsole — Betrieb läuft weiter.
            tracing_subscriber::registry()
                .with(filter)
                .with(stderr_layer)
                .init();
            tracing::warn!("Log-Datei '{log_file}' nicht öffenbar: {e} — nur stderr");
            None
        }
    }
}
