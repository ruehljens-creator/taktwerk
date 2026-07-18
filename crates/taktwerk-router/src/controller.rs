//! IS-05-**Controller**-Client: setzt/löst Koppelpunkte, indem er die
//! Connection-API eines (fremden) NMOS-Receivers ansteuert.
//!
//! Minimaler HTTP/1.1-`PATCH`-Client (tokio-TCP, keine schwere HTTP-Dep) — genau
//! so viel, wie ein NMOS-`PATCH …/receivers/{id}/staged` (activate_immediate)
//! braucht. Das ist die „Kreuzschiene setzt Koppelpunkt X→Y"-Aktion.

use std::io;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Verbindet den Receiver `receiver_id` (unter `host:port`) mit einem Sender —
/// beschrieben durch dessen `sdp`. Gibt den HTTP-Statuscode zurück.
pub async fn connect_receiver(
    host: &str,
    port: u16,
    receiver_id: &str,
    sdp: &str,
) -> io::Result<u16> {
    let body = json!({
        "master_enable": true,
        "activation": { "mode": "activate_immediate" },
        "transport_file": { "data": sdp, "type": "application/sdp" }
    })
    .to_string();
    patch(host, port, &staged_path(receiver_id), &body).await
}

/// Löst den Koppelpunkt (master_enable=false).
pub async fn disconnect_receiver(host: &str, port: u16, receiver_id: &str) -> io::Result<u16> {
    let body = json!({
        "master_enable": false,
        "activation": { "mode": "activate_immediate" }
    })
    .to_string();
    patch(host, port, &staged_path(receiver_id), &body).await
}

fn staged_path(receiver_id: &str) -> String {
    format!("/x-nmos/connection/v1.1/single/receivers/{receiver_id}/staged")
}

/// Führt einen HTTP/1.1-`GET` aus und liefert den Response-Body (für IS-04:
/// Sender-/Receiver-Listen einer fremden NMOS-Node-API abfragen).
pub async fn get_json(host: &str, port: u16, path: &str) -> io::Result<String> {
    let mut stream = TcpStream::connect((host, port)).await?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    let mut resp = Vec::with_capacity(2048);
    stream.read_to_end(&mut resp).await?;
    let text = String::from_utf8_lossy(&resp);
    // Body = alles nach dem Header-Ende.
    match text.split_once("\r\n\r\n") {
        Some((_h, body)) => Ok(body.to_string()),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "HTTP: kein Body",
        )),
    }
}

/// Führt einen HTTP/1.1-`PATCH` mit JSON-Body aus und liefert den Statuscode.
async fn patch(host: &str, port: u16, path: &str, body: &str) -> io::Result<u16> {
    let mut stream = TcpStream::connect((host, port)).await?;
    let req = format!(
        "PATCH {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await?;
    let mut resp = Vec::with_capacity(1024);
    stream.read_to_end(&mut resp).await?;
    let text = String::from_utf8_lossy(&resp);
    let code = text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "keine HTTP-Statuszeile"))?;
    Ok(code)
}
