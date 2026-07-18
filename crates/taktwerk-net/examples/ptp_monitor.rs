//! PTP-Monitor: lauscht auf IEEE-1588-Multicast (224.0.1.129, Ports 319/320),
//! zeigt Announce-Grandmaster (via BMCA gewählt) und Sync/Follow_Up an.
//!
//! Interop-Test gegen einen echten PTP-Master (z. B. linuxptp `ptp4l`):
//!   cargo run -p taktwerk-net --example ptp_monitor [INTERFACE-IP]
//! Läuft, bis `SECONDS` (Default 8) verstrichen sind; Exit 0, wenn mindestens
//! eine Announce empfangen wurde.

use std::net::Ipv4Addr;
use std::time::Duration;

use taktwerk_core::ptp::{BmcaOrder, ClockDataset};
use taktwerk_net::{PtpListener, PtpMessage};

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
    let secs: u64 = std::env::var("SECONDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8);

    let mut listener = PtpListener::bind(iface)?;
    println!("PTP-Monitor auf {iface} — lausche {secs}s auf 224.0.1.129:319/320 …");

    let mut best_gm: Option<ClockDataset> = None;
    let mut announces = 0u32;
    let mut syncs = 0u32;
    let mut followups = 0u32;

    let deadline = tokio::time::sleep(Duration::from_secs(secs));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            res = listener.recv() => {
                let (msg, from) = res?;
                match msg {
                    PtpMessage::Announce(a) => {
                        announces += 1;
                        let ds = a.to_clock_dataset();
                        let better = match &best_gm {
                            None => true,
                            Some(cur) => matches!(ClockDataset::compare(&ds, cur), BmcaOrder::ABetter),
                        };
                        if better {
                            best_gm = Some(ds);
                            println!(
                                "Announce von {from}: GM {:02X?} class={} prio1={} → neuer bester Master (BMCA)",
                                a.gm_identity, a.gm_clock_class, a.gm_priority1
                            );
                        }
                    }
                    PtpMessage::Sync(s) => {
                        syncs += 1;
                        if syncs <= 3 {
                            println!("Sync #{syncs}: seq={} ts={}ns", s.header.sequence_id, s.timestamp.total_nanos());
                        }
                    }
                    PtpMessage::FollowUp(f) => {
                        followups += 1;
                        if followups <= 3 {
                            println!("Follow_Up #{followups}: seq={} preciseTs={}ns", f.header.sequence_id, f.timestamp.total_nanos());
                        }
                    }
                    PtpMessage::Other(h) => {
                        let _ = h;
                    }
                }
            }
        }
    }

    println!("--- Bilanz: {announces} Announce, {syncs} Sync, {followups} Follow_Up ---");
    match best_gm {
        Some(gm) => {
            println!(
                "Gewählter Grandmaster: {:02X?} (class {})",
                gm.clock_identity, gm.clock_class
            );
            Ok(())
        }
        None => {
            eprintln!("Keine Announce empfangen — läuft ein PTP-Master im Netz/Interface?");
            std::process::exit(1);
        }
    }
}
