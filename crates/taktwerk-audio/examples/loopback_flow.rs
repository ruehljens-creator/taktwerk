//! Selbsttest eines **virtuellen Loopback-Geräts** (Linux `snd-aloop`): schreibt
//! einen Ton über Taktwerks [`CpalBackend`] auf ein Ausgabegerät und liest ihn
//! über ein zweites [`CpalBackend`] vom gekabelten Eingang zurück. Beweist, dass
//! echtes Audio durch das virtuelle Gerät in Taktwerks Aufnahmepfad fließt.
//!
//! Beide Enden laufen mit demselben Profil (48 kHz / 2 ch) — genau die AES67-Rate,
//! die `snd-aloop` an beiden Kabelenden verlangt.
//!
//!   cargo run -p taktwerk-audio --features cpal-backend --example loopback_flow \
//!     -- "Loopback,DEV=1" "Loopback,DEV=0"

use taktwerk_audio::{AudioBackend, CpalBackend};
use taktwerk_core::StreamProfile;

fn main() {
    let out_name = std::env::args().nth(1).unwrap_or_else(|| "Loopback,DEV=1".into());
    let in_name = std::env::args().nth(2).unwrap_or_else(|| "Loopback,DEV=0".into());
    let p = StreamProfile::level_a(2); // 48 kHz, 2 ch

    // Playback zuerst öffnen (pinnt das snd-aloop-Kabel auf 48 kHz/2 ch), dann Capture.
    let mut out = CpalBackend::with_devices(p, false, true, None, Some(out_name.clone()))
        .expect("Playback-Gerät öffnen");
    let mut inp = CpalBackend::with_devices(p, true, false, Some(in_name.clone()), None)
        .expect("Capture-Gerät öffnen");
    println!("Playback: {out_name}  ←kabel→  Capture: {in_name}");

    let per = p.frames_per_packet() as usize; // 48 Frames/Block
    let sr = p.sample_rate as f32;
    let mut ph = 0f32;
    let mut nonzero = 0u64;
    let mut total = 0u64;

    for _ in 0..600 {
        // 440-Hz-Sinus als i32 (linksbündig) erzeugen und ausgeben.
        let mut block = vec![0i32; per * 2];
        for fr in block.chunks_mut(2) {
            let s = ((ph * std::f32::consts::TAU).sin() * 0.3 * 2_147_483_647.0) as i32;
            ph += 440.0 / sr;
            if ph >= 1.0 {
                ph -= 1.0;
            }
            fr[0] = s;
            fr[1] = s;
        }
        out.write_playback(&block).unwrap();

        let cap = inp.read_capture(per).unwrap();
        nonzero += cap.iter().filter(|&&x| x != 0).count() as u64;
        total += cap.len() as u64;
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    println!("Loopback-Fluss: {nonzero}/{total} Capture-Samples ungleich Null");
    if nonzero > 0 {
        println!("OK — echtes Audio floss durch das virtuelle Gerät in den Aufnahmepfad.");
    } else {
        println!("FEHLER — nichts empfangen (Kabel/Format/Rate prüfen).");
        std::process::exit(1);
    }
}
