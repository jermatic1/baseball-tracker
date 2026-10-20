//! Answers "where are you?" broadcasts so a simulator can find the socket
//! without knowing the tracker's address. A client broadcasts
//! `{"type": "discover", "version": 1}` to UDP port 7881 and the tracker
//! replies to the sender with the socket's port and path; the sender takes
//! the address from the reply's source.

use std::net::UdpSocket;

use serde::{Deserialize, Serialize};

pub const PORT: u16 = 7881;
const VERSION: u32 = 1;
const MAX_REQUEST: usize = 512;

#[derive(Deserialize)]
struct Request {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Serialize)]
struct Reply<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    version: u32,
    port: u16,
    path: &'static str,
    name: &'a str,
}

/// Bind the discovery port and answer requests on a background thread for
/// the socket served on `ws_port`.
pub fn spawn(ws_port: u16) -> std::io::Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", PORT))?;
    let name = hostname();
    std::thread::spawn(move || serve(&socket, ws_port, &name));
    Ok(())
}

fn serve(socket: &UdpSocket, ws_port: u16, name: &str) {
    let mut buf = [0u8; MAX_REQUEST];
    loop {
        let Ok((n, sender)) = socket.recv_from(&mut buf) else {
            continue;
        };
        if let Some(reply) = reply(&buf[..n], ws_port, name) {
            let _ = socket.send_to(&reply, sender);
        }
    }
}

/// The reply a discover request earns, or none for anything else.
fn reply(request: &[u8], ws_port: u16, name: &str) -> Option<Vec<u8>> {
    let request: Request = serde_json::from_slice(request).ok()?;
    if request.kind != "discover" {
        return None;
    }
    serde_json::to_vec(&Reply {
        kind: "tracker",
        version: VERSION,
        port: ws_port,
        path: "/ws",
        name,
    })
    .ok()
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_earns_the_socket_address() {
        let reply = reply(br#"{"type": "discover", "version": 1}"#, 7880, "jetson").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&reply).unwrap();
        assert_eq!(v["type"], "tracker");
        assert_eq!(v["version"], 1);
        assert_eq!(v["port"], 7880);
        assert_eq!(v["path"], "/ws");
        assert_eq!(v["name"], "jetson");
    }

    #[test]
    fn other_traffic_is_ignored() {
        assert!(reply(br#"{"type": "hello"}"#, 7880, "").is_none());
        assert!(reply(b"not json", 7880, "").is_none());
        assert!(reply(b"", 7880, "").is_none());
    }

    #[test]
    fn replies_go_to_the_sender() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || serve(&server, 7880, "test"));

        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        client
            .send_to(br#"{"type": "discover", "version": 1}"#, addr)
            .unwrap();
        let mut buf = [0u8; 512];
        let (n, from) = client.recv_from(&mut buf).unwrap();
        assert_eq!(from, addr);
        let v: serde_json::Value = serde_json::from_slice(&buf[..n]).unwrap();
        assert_eq!(v["type"], "tracker");
    }
}
