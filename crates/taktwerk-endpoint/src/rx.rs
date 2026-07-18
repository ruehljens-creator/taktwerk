//! RxStream — RTP-Recv → Playback.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use taktwerk_audio::AudioBackend;
use taktwerk_net::RtpReceiver;
use tokio::sync::watch;

use crate::audio_err;

/// Empfangs-Strom: nimmt RTP-Pakete entgegen und schreibt die dekodierten
/// Samples ins Audio-Backend (Netz → Playback). Der Paketzähler ist ein
/// `Arc<AtomicU64>`, damit ihn Aufrufer (z. B. der Daemon für REST-Status)
/// live mitlesen können, während [`RxStream::run`] die Struktur besitzt.
pub struct RxStream {
    receiver: RtpReceiver,
    backend: Box<dyn AudioBackend>,
    packets_recv: Arc<AtomicU64>,
}

impl RxStream {
    pub fn new(receiver: RtpReceiver, backend: Box<dyn AudioBackend>) -> Self {
        Self {
            receiver,
            backend,
            packets_recv: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Anzahl bisher empfangener/gespielter Pakete.
    pub fn packets_recv(&self) -> u64 {
        self.packets_recv.load(Ordering::Relaxed)
    }

    /// Teilbarer Live-Paketzähler (für externe Statusabfragen).
    pub fn packet_counter(&self) -> Arc<AtomicU64> {
        self.packets_recv.clone()
    }

    /// Empfängt genau ein Paket und schreibt es ins Backend.
    pub async fn pump_once(&mut self) -> io::Result<()> {
        let pkt = self.receiver.recv().await?;
        self.backend
            .write_playback(&pkt.samples)
            .map_err(audio_err)?;
        let n = self.packets_recv.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::trace!(seq = pkt.header.sequence, packets = n, "RX pump");
        Ok(())
    }

    /// Läuft, bis `shutdown` `true` meldet oder der Kanal schließt.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        loop {
            tokio::select! {
                pkt = self.receiver.recv() => {
                    let pkt = pkt?;
                    self.backend.write_playback(&pkt.samples).map_err(audio_err)?;
                    let n = self.packets_recv.fetch_add(1, Ordering::Relaxed) + 1;
                    tracing::trace!(seq = pkt.header.sequence, packets = n, "RX pump");
                }
                res = shutdown.changed() => {
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
    use taktwerk_core::StreamProfile;
    use taktwerk_net::RtpSender;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn rx_writes_received_frames_to_backend() {
        let profile = StreamProfile::level_a(2);

        let rx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let rx_addr = rx_sock.local_addr().unwrap();
        let receiver = RtpReceiver::new(rx_sock, profile);
        // NullBackend zählt geschriebene Frames → messbar.
        let backend = Box::new(NullBackend::new(profile));
        let mut rx = RxStream::new(receiver, backend);

        let tx_sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let mut tx = RtpSender::new(tx_sock, rx_addr, profile, 97, 7, 0);
        let per_pkt = profile.frames_per_packet() as usize * profile.channels as usize;
        tx.send_block(&vec![0i32; per_pkt]).await.unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), rx.pump_once())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rx.packets_recv(), 1);
    }
}
