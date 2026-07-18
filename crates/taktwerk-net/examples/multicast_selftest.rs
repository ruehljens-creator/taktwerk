//! Multicast-Selbsttest: sendet einen kleinen AES67-RTP-Stream in eine echte
//! Multicast-Gruppe und empfaengt ihn auf demselben Host wieder (loop=on).
//! Prueft den IGMP-Join + Multicast-Sende-/Empfangspfad nativ — im Gegensatz zu
//! den Unit-Tests, die ueber Unicast-Loopback laufen.
//!
//! Nutzung:
//!   cargo run -p taktwerk-net --example multicast_selftest [INTERFACE-IP]
//! Ohne Argument wird das Default-Interface benutzt. Exit-Code 0 = mindestens
//! ein Paket empfangen.

use std::net::Ipv4Addr;
use std::time::Duration;

use taktwerk_core::StreamProfile;
use taktwerk_net::{bind_receiver, bind_sender, MulticastConfig, RtpReceiver, RtpSender};

fn main() -> std::io::Result<()> {
    // Manuell gebaute Runtime (kein tokio-macros noetig).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run())
}

async fn run() -> std::io::Result<()> {
    let group = Ipv4Addr::new(239, 69, 83, 67); // AES67-typischer Bereich
    let port = 5004;
    let profile = StreamProfile::level_a(2);

    let iface = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<Ipv4Addr>().ok())
        .unwrap_or(Ipv4Addr::UNSPECIFIED);

    let cfg = MulticastConfig::new(group, port).with_interface(iface);
    println!(
        "Gruppe {}:{}  Interface {}  Profil {}ch/{}Hz/{}us",
        group,
        port,
        iface,
        profile.channels,
        profile.sample_rate,
        profile.ptime_us
    );

    // Empfaenger zuerst (Join), dann Sender mit aktiviertem Multicast-Loopback.
    let rx_sock = bind_receiver(&cfg)?;
    let mut rx = RtpReceiver::new(rx_sock, profile);
    let tx_sock = bind_sender(&cfg, true)?;
    let mut tx = RtpSender::new(tx_sock, cfg.dest(), profile, 97, 0xCAFE_F00D, 0);

    // Fuenf Pakete voll (Level A: je 48 Frames = 1 ms).
    let per_pkt = profile.frames_per_packet() as usize * profile.channels as usize;
    let block = vec![0i32; per_pkt * 5];
    tx.send_block(&block).await?;
    println!("gesendet: 5 RTP-Pakete → {}", cfg.dest());

    let mut got = 0;
    for _ in 0..5 {
        match tokio::time::timeout(Duration::from_millis(800), rx.recv()).await {
            Ok(Ok(pkt)) => {
                got += 1;
                println!(
                    "empfangen: seq={} ts={} ssrc={:#x} frames={} von {}",
                    pkt.header.sequence,
                    pkt.header.timestamp,
                    pkt.header.ssrc,
                    pkt.frames(profile.channels),
                    pkt.from
                );
            }
            Ok(Err(e)) => eprintln!("recv-Fehler: {e}"),
            Err(_) => break, // Timeout: keine weiteren Pakete
        }
    }

    println!("Multicast-Selbsttest: {got}/5 Pakete empfangen");
    if got == 0 {
        eprintln!("FEHLER: kein Paket empfangen (Interface/Routing pruefen)");
        std::process::exit(1);
    }
    Ok(())
}
