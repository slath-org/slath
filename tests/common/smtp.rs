// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A minimal SMTP server for tests: accepts plain SMTP, records every
//! message, and can be told to fail sessions temporarily (451).

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One received message.
#[derive(Debug, Clone)]
pub struct Received {
    pub from: String,
    pub to: Vec<String>,
    /// Headers and body as sent (dot-unstuffed), CRLF line ends.
    pub data: String,
}

impl Received {
    /// The value of the first header `name` (case-insensitive, unfolded).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<String> {
        let head = self.data.split("\r\n\r\n").next()?;
        let mut out: Option<String> = None;
        for line in head.split("\r\n") {
            if line.starts_with([' ', '\t']) {
                if let Some(v) = out.as_mut() {
                    v.push_str(line.trim_start());
                }
                continue;
            }
            if out.is_some() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case(name)
            {
                out = Some(v.trim().to_owned());
            }
        }
        out
    }
}

#[derive(Clone)]
pub struct SmtpServer {
    pub port: u16,
    messages: Arc<Mutex<Vec<Received>>>,
    fail_sessions: Arc<AtomicUsize>,
}

impl SmtpServer {
    #[must_use]
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = Self {
            port,
            messages: Arc::default(),
            fail_sessions: Arc::default(),
        };
        let s = server.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let s = s.clone();
                std::thread::spawn(move || {
                    let _ = s.session(conn);
                });
            }
        });
        server
    }

    /// `[smtp]` settings pointing at this server.
    #[must_use]
    pub fn config(&self) -> String {
        format!(
            "[smtp]\nhost = \"127.0.0.1\"\nport = {}\ntls = \"none\"\nfrom = \"Slath <noreply@example.org>\"\n",
            self.port
        )
    }

    /// The next `n` sessions answer 451 (try again later).
    pub fn fail_next(&self, n: usize) {
        self.fail_sessions.store(n, Ordering::SeqCst);
    }

    #[must_use]
    pub fn messages(&self) -> Vec<Received> {
        self.messages.lock().unwrap().clone()
    }

    /// Waits until at least `n` messages arrived.
    #[must_use]
    pub fn wait_for(&self, n: usize, timeout: Duration) -> Vec<Received> {
        let start = Instant::now();
        loop {
            let m = self.messages();
            if m.len() >= n {
                return m;
            }
            assert!(
                start.elapsed() < timeout,
                "expected {n} messages, got {}",
                m.len()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Waits for a message whose text contains `needle`.
    #[must_use]
    pub fn wait_for_text(&self, needle: &str, timeout: Duration) -> Received {
        let start = Instant::now();
        loop {
            if let Some(m) = self
                .messages()
                .into_iter()
                .find(|m| m.data.replace("=\r\n", "").contains(needle))
            {
                return m;
            }
            assert!(
                start.elapsed() < timeout,
                "no message containing {needle:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn session(&self, conn: TcpStream) -> std::io::Result<()> {
        conn.set_read_timeout(Some(Duration::from_secs(30)))?;
        let mut out = conn.try_clone()?;
        let mut reader = BufReader::new(conn);
        let fail = self
            .fail_sessions
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        out.write_all(b"220 test.invalid ESMTP\r\n")?;
        let (mut from, mut to) = (String::new(), Vec::new());
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                return Ok(());
            }
            let cmd = line.trim_end().to_owned();
            let upper = cmd.to_ascii_uppercase();
            if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                out.write_all(b"250-test.invalid\r\n250 8BITMIME\r\n")?;
            } else if upper.starts_with("MAIL FROM:") {
                if fail {
                    out.write_all(b"451 4.3.0 try again later\r\n")?;
                    continue;
                }
                from = cmd[10..].trim().to_owned();
                out.write_all(b"250 OK\r\n")?;
            } else if upper.starts_with("RCPT TO:") {
                to.push(cmd[8..].trim().to_owned());
                out.write_all(b"250 OK\r\n")?;
            } else if upper == "DATA" {
                out.write_all(b"354 go ahead\r\n")?;
                let mut data = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line)?;
                    if line == ".\r\n" || line == ".\n" {
                        break;
                    }
                    let l = line.strip_prefix('.').unwrap_or(&line);
                    data.push_str(l);
                }
                self.messages.lock().unwrap().push(Received {
                    from: std::mem::take(&mut from),
                    to: std::mem::take(&mut to),
                    data,
                });
                out.write_all(b"250 OK queued\r\n")?;
            } else if upper == "QUIT" {
                out.write_all(b"221 bye\r\n")?;
                return Ok(());
            } else {
                out.write_all(b"250 OK\r\n")?;
            }
        }
    }
}
