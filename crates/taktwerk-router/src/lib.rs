//! # taktwerk-router
//!
//! Die **NMOS-Control-Plane** von Taktwerk (§3.2 des Projektbriefs): stellt den
//! Knoten und seine AES67-Streams über die AMWA-NMOS-APIs bereit —
//! **IS-04 Node-API** (Discovery/Registrierung) und **IS-05 Connection-API**
//! (Verbindungssteuerung + SDP-Transportdatei).
//!
//! Aufteilung:
//! - [`ids`]       — deterministische UUIDs für stabile Ressourcen-Identität.
//! - [`resources`] — [`resources::NmosNode`]: Knoten → NMOS-Ressourcen + SDP.
//! - [`nmos`]      — die Axum-App ([`nmos::app`]) mit IS-04/IS-05-Endpunkten.
//!
//! Die App wird eigenständig ausgeliefert (eigener Port), sodass sie unabhängig
//! neben der Daemon-REST-API läuft und den Audiopfad nicht berührt (§4).

pub mod ids;
pub mod nmos;
pub mod resources;

pub use nmos::app;
pub use resources::NmosNode;
