//! SAP-Selbsttest: kündigt einen Stream in der well-known SAP-Gruppe
//! (239.255.255.255:9875) an und empfängt die eigene Ankündigung wieder,
//! inklusive geparster SDP-Beschreibung. Prüft den Discovery-Pfad nativ.
//!
//! Nutzung:
//!   cargo run -p taktwerk-net --example sap_selftest [INTERFACE-IP]
//! Exit 0 = eigene Ankündigung empfangen und SDP lesbar.

use std::net::Ipv4Addr;
use std::time::Duration;

use taktwerk_core::sdp::{AudioSession, PtpRefClock};
use taktwerk_core::StreamProfile;
use taktwerk_net::{bind_sap_announcer, bind_sap_listener, SapAnnouncer, SapListener};

fn main() -> std::io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(run())
}

async fn run() -> std::io::Result<()> {
    let iface = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<Ipv4Addr>().ok())
        .unwrap_or(Ipv4Addr::UNSPECIFIED);

    let session = AudioSession {
        session_name: "Taktwerk SAP-Selbsttest".into(),
        origin_unicast: "192.168.1.20".into(),
        multicast_addr: "239.69.83.67".into(),
        port: 5004,
        payload_type: 97,
        profile: StreamProfile::level_a(2),
        refclk: Some(PtpRefClock {
            gmid: "00-11-22-FF-FE-33-44-55".into(),
            domain: 0,
        }),
        mediaclk_offset: 0,
    };

    let listener_sock = bind_sap_listener(iface)?;
    let mut listener = SapListener::new(listener_sock);

    let announcer_sock = bind_sap_announcer(iface, true)?;
    let announcer = SapAnnouncer::new(announcer_sock, Ipv4Addr::new(192, 168, 1, 20), &session);
    announcer.announce().await?;
    println!(
        "angekündigt: \"{}\" (msg_id_hash={:#06x}) auf SAP 239.255.255.255:9875",
        session.session_name,
        announcer.msg_id_hash()
    );

    // Auf die eigene Ankündigung warten.
    loop {
        match tokio::time::timeout(Duration::from_millis(1500), listener.recv()).await {
            Ok(Ok(ev)) if ev.msg_id_hash == announcer.msg_id_hash() => {
                let s = ev.session.as_ref();
                println!(
                    "empfangen: announce={} von {} → {}",
                    ev.announce,
                    ev.from,
                    s.map(|s| format!(
                        "{} {}ch @ {}:{}",
                        s.session_name, s.profile.channels, s.multicast_addr, s.port
                    ))
                    .unwrap_or_else(|| "<SDP nicht lesbar>".into())
                );
                if s.is_some() {
                    println!("SAP-Selbsttest: OK");
                    return Ok(());
                }
            }
            Ok(Ok(_)) => continue, // fremde Ankündigung, weiterhören
            Ok(Err(e)) => eprintln!("recv-Fehler: {e}"),
            Err(_) => {
                eprintln!("FEHLER: eigene Ankündigung nicht empfangen (Interface/Routing?)");
                std::process::exit(1);
            }
        }
    }
}
