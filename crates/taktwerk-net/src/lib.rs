//! # taktwerk-net
//!
//! Die **Netz-Schicht** von Taktwerk: Multicast-UDP-Sockets und der RTP-
//! Sender/Receiver, die aus dem plattformneutralen Kern ([`taktwerk_core`])
//! echte AES67-Streams machen.
//!
//! Aufteilung (bewusst klein und je einem Zweck zugeordnet):
//! - [`multicast`] — Socket-Erzeugung, IGMP-Join/Leave, Interface-/TTL-Wahl.
//!   Kapselt die einzigen leicht OS-abhaengigen Socket-Optionen (via `socket2`),
//!   damit Sender/Receiver selbst OS-neutral bleiben.
//! - [`sender`]    — [`sender::RtpSender`]: interleavte Samples → RTP-Pakete → Netz.
//! - [`receiver`]  — [`receiver::RtpReceiver`]: Netz → RTP-Parse → Samples.
//!
//! Sender und Receiver arbeiten gegen eine beliebige [`std::net::SocketAddr`]
//! (Multicast **oder** Unicast) — das macht die RTP-Framing-Pipeline ohne
//! Multicast-Routing testbar und die Multicast-Einrichtung zu einer reinen
//! Socket-Frage in [`multicast`].

pub mod multicast;
pub mod receiver;
pub mod sender;

pub use multicast::{MulticastConfig, bind_receiver, bind_sender};
pub use receiver::{ReceivedPacket, RtpReceiver};
pub use sender::RtpSender;
