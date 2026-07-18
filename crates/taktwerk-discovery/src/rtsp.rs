//! Minimaler RTSP (RFC 2326) für RAVENNA-Session-Beschreibung.
//!
//! RAVENNA kündigt Sessions per mDNS an und liefert ihre **SDP** über RTSP
//! `DESCRIBE`. Hier steckt genau so viel RTSP, wie dafür nötig ist:
//! - [`describe`] — Client: holt die SDP einer Session (`DESCRIBE`).
//! - [`handle_request`] — Server-Seite: beantwortet `OPTIONS`/`DESCRIBE` mit der
//!   eigenen SDP (der Server-Loop selbst liegt im Daemon).

use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Holt die SDP einer RAVENNA-Session per RTSP `DESCRIBE`.
pub async fn describe(host: &str, port: u16, path: &str) -> io::Result<String> {
    let mut stream = TcpStream::connect((host, port)).await?;
    let url = format!("rtsp://{host}:{port}{path}");
    let req = format!(
        "DESCRIBE {url} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\nUser-Agent: Taktwerk\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;

    // Antwort lesen (Header + Body). RTSP-SDP-Antworten sind klein.
    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(body) = extract_body(&buf) {
            return Ok(body);
        }
        if buf.len() > 64 * 1024 {
            break; // Schutz gegen Endlos-/Riesenantworten
        }
    }
    // Kein Content-Length: alles nach dem Header-Ende als Body werten.
    extract_body_lenient(&buf)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "RTSP: keine SDP im DESCRIBE"))
}

/// Body extrahieren, sobald `Content-Length` Bytes nach dem Header da sind.
fn extract_body(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    let sep = text.find("\r\n\r\n")?;
    let headers = &text[..sep];
    let body_start = sep + 4;
    let content_len = headers
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse::<usize>().ok())
        })
        .flatten()?;
    if buf.len() - body_start >= content_len {
        Some(text[body_start..body_start + content_len].to_string())
    } else {
        None
    }
}

/// Fallback: Body = alles nach dem ersten Header-Ende (ohne Content-Length).
fn extract_body_lenient(buf: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(buf);
    let sep = text.find("\r\n\r\n")?;
    Some(text[sep + 4..].to_string())
}

/// Baut eine RTSP-Antwort auf eine eingehende Anfrage. Beantwortet `OPTIONS`
/// (Methodenliste) und `DESCRIBE` (SDP aus `sdp`); sonst 501.
pub fn handle_request(request: &str, sdp: &str) -> String {
    let mut lines = request.lines();
    let start = lines.next().unwrap_or("");
    let method = start.split_whitespace().next().unwrap_or("");
    let cseq = request
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("cseq:")
                .map(|v| v.trim().to_string())
        })
        .unwrap_or_else(|| "0".to_string());

    match method {
        "OPTIONS" => format!(
            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nPublic: OPTIONS, DESCRIBE\r\n\r\n"
        ),
        "DESCRIBE" => format!(
            "RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",
            sdp.len()
        ),
        _ => format!("RTSP/1.0 501 Not Implemented\r\nCSeq: {cseq}\r\n\r\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_response_carries_sdp() {
        let sdp = "v=0\r\ns=Test\r\n";
        let resp = handle_request("DESCRIBE rtsp://h/p RTSP/1.0\r\nCSeq: 4\r\n\r\n", sdp);
        assert!(resp.starts_with("RTSP/1.0 200 OK"));
        assert!(resp.contains("CSeq: 4"));
        assert!(resp.contains("Content-Type: application/sdp"));
        assert!(resp.contains(&format!("Content-Length: {}", sdp.len())));
        assert!(resp.ends_with(sdp));
    }

    #[test]
    fn options_lists_methods() {
        let resp = handle_request("OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n", "");
        assert!(resp.contains("Public: OPTIONS, DESCRIBE"));
    }

    #[test]
    fn client_extracts_body_with_content_length() {
        let sdp = "v=0\r\ns=X\r\n";
        let msg = format!(
            "RTSP/1.0 200 OK\r\nCSeq: 1\r\nContent-Length: {}\r\n\r\n{sdp}",
            sdp.len()
        );
        assert_eq!(extract_body(msg.as_bytes()).as_deref(), Some(sdp));
    }

    #[test]
    fn client_waits_for_full_body() {
        // Content-Length 10, aber nur 3 Body-Bytes da → None (noch warten).
        let msg = "RTSP/1.0 200 OK\r\nContent-Length: 10\r\n\r\nabc";
        assert!(extract_body(msg.as_bytes()).is_none());
    }
}
