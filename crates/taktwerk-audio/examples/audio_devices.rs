//! Listet die echten Audiogeräte und öffnet kurz ein cpal-Backend (Capture +
//! Playback), um zu zeigen, dass die Streams auf echter Hardware laufen.
//!
//!   cargo run -p taktwerk-audio --features cpal-backend --example audio_devices

use taktwerk_audio::{list_devices, AudioBackend, CpalBackend};
use taktwerk_core::StreamProfile;

fn main() {
    let (inputs, outputs) = list_devices();
    println!("Eingabegeräte ({}):", inputs.len());
    for d in &inputs {
        println!("  · {d}");
    }
    println!("Ausgabegeräte ({}):", outputs.len());
    for d in &outputs {
        println!("  · {d}");
    }

    let profile = StreamProfile::level_a(2);
    match CpalBackend::new(profile, true, true) {
        Ok(mut be) => {
            println!(
                "cpal-Backend geöffnet: {} · {}ch/{}Hz",
                be.name(),
                profile.channels,
                profile.sample_rate
            );
            // Kurz laufen lassen: Capture lesen (Mic-Aktivität?), Stille abspielen.
            let per = profile.frames_per_packet() as usize;
            let mut nonzero = 0u64;
            for _ in 0..200 {
                let block = be.read_capture(per).unwrap();
                nonzero += block.iter().filter(|&&s| s != 0).count() as u64;
                be.write_playback(&vec![0i32; per * profile.channels as usize])
                    .unwrap();
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            println!("Capture-Samples ungleich Null in ~200ms: {nonzero} (Mic-Signal, falls > 0)");
            println!("OK — Ein-/Ausgabe-Streams liefen ohne Fehler.");
        }
        Err(e) => {
            println!("cpal-Backend nicht verfügbar: {e}");
            println!("(kein Audiogerät? — dann greift der NullBackend-Fallback)");
        }
    }
}
