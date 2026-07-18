//! Spielt einen Sinuston (440 Hz) auf ein per **Name** gewähltes Ausgabegerät —
//! Test-Signalquelle für virtuelle Geräte (Linux `snd-aloop`, macOS Pro Tools
//! Audio Bridge, …). Damit lässt sich prüfen, dass echtes Audio durch ein
//! virtuelles Gerät in Taktwerks Aufnahmepfad fließt.
//!
//!   cargo run -p taktwerk-audio --features cpal-backend --example tone -- "Loopback,DEV=1" 30
//!
//! Argument 1 = Teilstring des Ausgabegeräts, Argument 2 = Dauer in s (Default 30).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;

/// Nächster Sinus-Sample; schreibt die Phase fort (0..1).
fn sine(phase: &mut f32, sr: f32) -> f32 {
    let s = (*phase * std::f32::consts::TAU).sin() * 0.3;
    *phase += 440.0 / sr;
    if *phase >= 1.0 {
        *phase -= 1.0;
    }
    s
}

fn main() {
    let want = std::env::args().nth(1).unwrap_or_default();
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let host = cpal::default_host();
    let dev = host
        .output_devices()
        .expect("Ausgabegeräte")
        .find(|d| d.name().map(|n| n.contains(&want)).unwrap_or(false))
        .unwrap_or_else(|| {
            eprintln!("kein Ausgabegerät mit Name ~ \"{want}\"");
            std::process::exit(1);
        });
    println!("Ausgabe: {}", dev.name().unwrap_or_default());

    let sc = dev.default_output_config().expect("default_output_config");
    let fmt = sc.sample_format();
    let cfg: cpal::StreamConfig = sc.into();
    let sr = cfg.sample_rate.0 as f32;
    let ch = cfg.channels as usize;
    let err = |e| eprintln!("Stream-Fehler: {e}");

    // Jede Format-Variante hält ihre eigene Phase (getrennte move-Closures).
    let stream = match fmt {
        SampleFormat::F32 => {
            let mut ph = 0f32;
            dev.build_output_stream(
                &cfg,
                move |d: &mut [f32], _| {
                    for fr in d.chunks_mut(ch) {
                        let s = sine(&mut ph, sr);
                        fr.iter_mut().for_each(|x| *x = s);
                    }
                },
                err,
                None,
            )
        }
        SampleFormat::I16 => {
            let mut ph = 0f32;
            dev.build_output_stream(
                &cfg,
                move |d: &mut [i16], _| {
                    for fr in d.chunks_mut(ch) {
                        let s = (sine(&mut ph, sr) * i16::MAX as f32) as i16;
                        fr.iter_mut().for_each(|x| *x = s);
                    }
                },
                err,
                None,
            )
        }
        SampleFormat::I32 => {
            let mut ph = 0f32;
            dev.build_output_stream(
                &cfg,
                move |d: &mut [i32], _| {
                    for fr in d.chunks_mut(ch) {
                        let s = (sine(&mut ph, sr) * i32::MAX as f32) as i32;
                        fr.iter_mut().for_each(|x| *x = s);
                    }
                },
                err,
                None,
            )
        }
        other => {
            eprintln!("Sampleformat nicht unterstützt: {other:?}");
            std::process::exit(1);
        }
    }
    .expect("Ausgabe-Stream");
    stream.play().expect("play");
    println!("Ton läuft {secs}s ({ch}ch @ {sr}Hz, {fmt:?}) …");
    std::thread::sleep(std::time::Duration::from_secs(secs));
}
