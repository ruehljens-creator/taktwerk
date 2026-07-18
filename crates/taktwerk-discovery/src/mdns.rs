//! mDNS/DNS-SD für RAVENNA-Sessions (browse + register) via `mdns-sd`.
//!
//! RAVENNA kündigt Audio-Sessions als **RTSP-Dienst** mit dem Subtyp
//! `_ravenna_session` an (`_ravenna_session._sub._rtsp._tcp.local.`). Wir browsen
//! diesen Subtyp, um fremde Sessions zu finden, und registrieren unseren eigenen
//! Stream unter demselben Typ, damit RAVENNA-Controller uns finden.
//!
//! `mdns-sd` läuft in einem eigenen Thread; wir überbrücken seine Events per
//! `tokio::mpsc` in die async-Welt des Daemons.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

/// DNS-SD-Basistyp (RTSP), unter dem RAVENNA-Sessions laufen.
pub const RAVENNA_SERVICE: &str = "_rtsp._tcp.local.";
/// RAVENNA-Subtyp für Audio-Sessions.
pub const RAVENNA_SUBTYPE: &str = "_ravenna_session._sub._rtsp._tcp.local.";
/// DNS-SD-Typ für NMOS-Node-APIs (IS-04).
pub const NMOS_NODE_SERVICE: &str = "_nmos-node._tcp.local.";

/// Ein generisch aufgelöster mDNS-Dienst.
#[derive(Debug, Clone)]
pub struct ResolvedService {
    pub instance: String,
    pub host: String,
    pub addr: Option<IpAddr>,
    pub port: u16,
    pub txt: HashMap<String, String>,
}

/// Eine per mDNS gefundene (oder von uns angebotene) RAVENNA-Session.
#[derive(Debug, Clone)]
pub struct RavennaSession {
    /// Voller DNS-SD-Instanzname.
    pub instance: String,
    /// Hostname (ohne abschließenden Punkt).
    pub host: String,
    /// Aufgelöste IP (falls vorhanden).
    pub addr: Option<IpAddr>,
    /// RTSP-Port.
    pub port: u16,
    /// RTSP-Pfad zur SDP (aus TXT `path` oder abgeleitet).
    pub path: String,
}

/// mDNS-Discovery-Handle (hält den `ServiceDaemon` am Leben). Klonbar — der
/// `ServiceDaemon` ist ein geteiltes Handle, sodass Browse und Register denselben
/// mDNS-Dienst nutzen.
#[derive(Clone)]
pub struct MdnsDiscovery {
    daemon: ServiceDaemon,
}

impl MdnsDiscovery {
    pub fn new() -> Result<Self, mdns_sd::Error> {
        Ok(Self {
            daemon: ServiceDaemon::new()?,
        })
    }

    /// Browst nach RAVENNA-Sessions. Liefert je **aufgelöster** Session ein
    /// [`RavennaSession`] über den zurückgegebenen Kanal.
    pub fn browse(&self) -> Result<mpsc::UnboundedReceiver<RavennaSession>, mdns_sd::Error> {
        let receiver = self.daemon.browse(RAVENNA_SUBTYPE)?;
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if let ServiceEvent::ServiceResolved(info) = event {
                    let addr = info.get_addresses().iter().next().copied();
                    let path = info
                        .get_property_val_str("path")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| default_path(info.get_fullname()));
                    let session = RavennaSession {
                        instance: info.get_fullname().to_string(),
                        host: info.get_hostname().trim_end_matches('.').to_string(),
                        addr,
                        port: info.get_port(),
                        path,
                    };
                    if tx.send(session).is_err() {
                        break; // Empfänger weg → Thread beenden
                    }
                }
            }
        });
        Ok(rx)
    }

    /// Bietet den eigenen Stream als RAVENNA-Session an (mDNS-Register).
    /// `instance` = Anzeigename, `host` = Hostname (z. B. "taktwerk"),
    /// `addr`/`port` = RTSP-Endpunkt, `path` = RTSP-Pfad zur SDP.
    pub fn register_session(
        &self,
        instance: &str,
        host: &str,
        addr: Ipv4Addr,
        port: u16,
        path: &str,
    ) -> Result<(), mdns_sd::Error> {
        let host_fqdn = format!("{host}.local.");
        let props = [("path", path), ("format", "AES67-L24")];
        let info = ServiceInfo::new(
            RAVENNA_SUBTYPE,
            instance,
            &host_fqdn,
            IpAddr::V4(addr),
            port,
            &props[..],
        )?;
        self.daemon.register(info)
    }

    /// Zieht eine zuvor registrierte Session zurück.
    pub fn unregister(&self, instance: &str) -> Result<(), mdns_sd::Error> {
        let fullname = format!("{instance}.{RAVENNA_SUBTYPE}");
        self.daemon.unregister(&fullname).map(|_| ())
    }

    /// Generischer Browse für einen DNS-SD-Diensttyp; liefert je aufgelöstem
    /// Dienst ein [`ResolvedService`].
    pub fn browse_type(
        &self,
        service_type: &str,
    ) -> Result<mpsc::UnboundedReceiver<ResolvedService>, mdns_sd::Error> {
        let receiver = self.daemon.browse(service_type)?;
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            while let Ok(event) = receiver.recv() {
                if let ServiceEvent::ServiceResolved(info) = event {
                    let mut txt = HashMap::new();
                    for p in info.get_properties().iter() {
                        txt.insert(p.key().to_string(), p.val_str().to_string());
                    }
                    let svc = ResolvedService {
                        instance: info.get_fullname().to_string(),
                        host: info.get_hostname().trim_end_matches('.').to_string(),
                        addr: info.get_addresses().iter().next().copied(),
                        port: info.get_port(),
                        txt,
                    };
                    if tx.send(svc).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(rx)
    }

    /// Browst NMOS-Node-APIs (`_nmos-node._tcp`).
    pub fn browse_nmos_nodes(
        &self,
    ) -> Result<mpsc::UnboundedReceiver<ResolvedService>, mdns_sd::Error> {
        self.browse_type(NMOS_NODE_SERVICE)
    }

    /// Registriert den eigenen Knoten als NMOS-Node-API (IS-04-Discovery).
    pub fn register_nmos_node(
        &self,
        instance: &str,
        host: &str,
        addr: Ipv4Addr,
        port: u16,
    ) -> Result<(), mdns_sd::Error> {
        let host_fqdn = format!("{host}.local.");
        let props = [
            ("api_proto", "http"),
            ("api_ver", "v1.3"),
            ("api_auth", "false"),
        ];
        let info = ServiceInfo::new(
            NMOS_NODE_SERVICE,
            instance,
            &host_fqdn,
            IpAddr::V4(addr),
            port,
            &props[..],
        )?;
        self.daemon.register(info)
    }
}

/// Abgeleiteter RTSP-Pfad, wenn kein TXT-`path` vorliegt: `/by-name/<instanz>`.
fn default_path(fullname: &str) -> String {
    let instance = fullname.split('.').next().unwrap_or(fullname);
    format!("/by-name/{instance}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_from_fullname() {
        assert_eq!(
            default_path("Kamera 3._ravenna_session._sub._rtsp._tcp.local."),
            "/by-name/Kamera 3"
        );
    }

    #[test]
    fn daemon_constructs() {
        // mDNS-Daemon muss sich erzeugen lassen (bindet 5353 mit REUSE).
        assert!(MdnsDiscovery::new().is_ok());
    }
}
