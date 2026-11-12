// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath healthcheck`: a container health check for Docker, Podman, and
//! Quadlet that needs no curl in the image. It asks the local server's
//! `/-/readyz` (exit 0 when ready), or with `--ssh` checks that sshd
//! greets with an SSH banner. It reads no configuration and no secrets.

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(5);

/// The address to probe: `addr`, else `SLATH__SERVER__LISTEN` (an
/// unspecified host becomes loopback), else the default 127.0.0.1:3000.
fn http_addr(addr: Option<&str>) -> Result<SocketAddr, String> {
    let raw = addr
        .map(str::to_owned)
        .or_else(|| std::env::var("SLATH__SERVER__LISTEN").ok())
        .unwrap_or_else(|| "127.0.0.1:3000".to_owned());
    let mut a = resolve(&raw)?;
    if a.ip().is_unspecified() {
        a.set_ip(if a.is_ipv4() {
            std::net::Ipv4Addr::LOCALHOST.into()
        } else {
            std::net::Ipv6Addr::LOCALHOST.into()
        });
    }
    Ok(a)
}

fn resolve(raw: &str) -> Result<SocketAddr, String> {
    raw.to_socket_addrs()
        .map_err(|e| format!("{raw}: {e}"))?
        .next()
        .ok_or_else(|| format!("{raw}: no address"))
}

fn connect(addr: SocketAddr) -> Result<TcpStream, String> {
    let s = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| format!("{addr}: {e}"))?;
    s.set_read_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    Ok(s)
}

/// `GET /-/readyz`: ready when the status is 200.
fn probe_http(addr: SocketAddr) -> Result<(), String> {
    let mut s = connect(addr)?;
    write!(
        s,
        "GET /-/readyz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nUser-Agent: slath-healthcheck\r\n\r\n"
    )
    .map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    match line.split_whitespace().nth(1) {
        Some("200") => Ok(()),
        Some(code) => Err(format!("not ready (HTTP {code})")),
        None => Err("no HTTP answer".to_owned()),
    }
}

/// An SSH server greets first with `SSH-2.0-…`.
fn probe_ssh(addr: SocketAddr) -> Result<(), String> {
    let mut line = String::new();
    BufReader::new(connect(addr)?)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    if line.starts_with("SSH-2.0-") {
        Ok(())
    } else {
        Err("no SSH banner".to_owned())
    }
}

pub fn run(addr: Option<&str>, ssh: Option<&str>) -> ExitCode {
    let result = match ssh {
        Some(raw) => resolve(raw).and_then(probe_ssh),
        None => http_addr(addr).and_then(probe_http),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            eprintln!("unhealthy: {why}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpListener;

    fn serve_once(answer: &'static [u8]) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            if !answer.starts_with(b"SSH") {
                let mut buf = [0u8; 512];
                let _ = c.read(&mut buf);
            }
            c.write_all(answer).unwrap();
        });
        a
    }

    #[test]
    fn ready_unready_and_ssh() {
        assert!(probe_http(serve_once(b"HTTP/1.1 200 OK\r\n\r\nready\n")).is_ok());
        assert_eq!(
            probe_http(serve_once(b"HTTP/1.1 503 Service Unavailable\r\n\r\n")),
            Err("not ready (HTTP 503)".to_owned())
        );
        assert!(probe_ssh(serve_once(b"SSH-2.0-OpenSSH_9.9\r\n")).is_ok());
        assert!(probe_ssh(serve_once(b"HTTP/1.1 200 OK\r\n")).is_err());
        // Nothing listening.
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = l.local_addr().unwrap();
        drop(l);
        assert!(probe_http(a).is_err());
    }

    #[test]
    fn unspecified_hosts_become_loopback() {
        assert_eq!(
            http_addr(Some("0.0.0.0:3000")).unwrap(),
            "127.0.0.1:3000".parse().unwrap()
        );
        assert_eq!(
            http_addr(Some("[::]:3000")).unwrap(),
            "[::1]:3000".parse().unwrap()
        );
    }
}
