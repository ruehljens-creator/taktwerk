//! TxStream — Capture → RTP-Send im ptime-Takt.

use std::io;
use std::time::Duration;

use taktwerk_audio::AudioBackend;
use taktwerk_core::clock::TimeSource;
use taktwerk_core::StreamProfile;
use taktwerk_net::RtpSender;
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};

use crate::audio_err;

/// Sende-Strom: zieht in jedem Paket-Intervall genau ein Paket (frames_per_packet)
/// aus dem Audio-Backend und schickt es über den RTP-Sender.
pub struct TxStream {
    backend: Box<dyn AudioBackend>,
    sender: RtpSender,
    frames_per_packet: usize,
    ptime: Duration,
    packets_sent: u64,
}

impl TxStream {
    /// Baut den Strom. Der RTP-Start-Timestamp wird aus `clock` gesetzt (Media-
    /// Clock-Ausrichtung); der Fortlauf pro Paket macht der [`RtpSender`] selbst.
    pub fn new(
        backend: Box<dyn AudioBackend>,
        socket: tokio::net::UdpSocket,
        dest: std::net::SocketAddr,
        profile: StreamProfile,
        payload_type: u8,
        ssrc: u32,
        clock: &dyn TimeSource,
    ) -> Self {
        let ts_start = clock.rtp_timestamp(profile.sample_rate);
        let sender = RtpSender::new(socket, dest, profile, payload_type, ssrc, ts_start);
        Self {
            backend,
            sender,
            frames_per_packet: profile.frames_per_packet() as usize,
            ptime: Duration::from_micros(profile.ptime_us as u64),
            packets_sent: 0,
        }
    }

    /// Anzahl bisher gesendeter Pakete.
    pub fn packets_sent(&self) -> u64 {
        self.packets_sent
    }

    /// Liest genau ein Paket aus dem Backend und sendet es als ein RTP-Paket.
    pub async fn pump_once(&mut self) -> io::Result<()> {
        let block = self
            .backend
            .read_capture(self.frames_per_packet)
            .map_err(audio_err)?;
        self.sender.send_block(&block).await?;
        self.packets_sent += 1;
        tracing::trace!(packets = self.packets_sent, "TX pump");
        Ok(())
    }

    /// Läuft im ptime-Takt, bis `shutdown` `true` meldet. Verpasste Ticks (unter
    /// Last) werden nicht nachgeholt, sondern verzögert — kein Burst.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        let mut ticker = interval(self.ptime);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => self.pump_once().await?,
                res = shutdown.changed() => {
                    // Sender beendet, wenn Kanal true meldet oder geschlossen wird.
                    if res.is_err() || *shutdown.borrow() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use taktwerk_audio::NullBackend;
    use taktwerk_core::clock::FixedTimeSource;
    use taktwerk_net::RtpReceiver;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn tx_pumps_packets_to_receiver() {
        let profile = StreamProfile::level_a(2);

        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let mut rx = RtpReceiver::new(rx_sock, profile);

        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let clock = FixedTimeSource(1_000_000_000); // 1 s → ts 48000
        let mut tx = TxStream::new(
            Box::new(NullBackend::new(profile)),
            tx_sock,
            rx_addr,
            profile,
            97,
            0xAA55,
            &clock,
        );

        for _ in 0..3 {
            tx.pump_once().await.unwrap();
        }
        assert_eq!(tx.packets_sent(), 3);

        // Erstes Paket muss den aus der Clock gesetzten Start-Timestamp tragen.
        let pkt = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pkt.header.timestamp, 48_000);
        assert_eq!(
            pkt.frames(profile.channels),
            profile.frames_per_packet() as usize
        );
    }

    #[tokio::test]
    async fn run_stops_on_shutdown() {
        let profile = StreamProfile::level_a(2);
        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let clock = FixedTimeSource(0);
        let tx = TxStream::new(
            Box::new(NullBackend::new(profile)),
            tx_sock,
            rx_addr,
            profile,
            96,
            1,
            &clock,
        );

        let (stop_tx, stop_rx) = watch::channel(false);
        let handle = tokio::spawn(tx.run(stop_rx));
        // Kurz laufen lassen, dann stoppen.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        stop_tx.send(true).unwrap();
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("run beendet nicht")
            .unwrap();
        assert!(res.is_ok());
    }
}
