// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Outbound email over SMTP.
//!
//! Every email goes through the `email.send` job (retries with backoff, at
//! most 8 attempts). Bodies can carry single-use links, so the job payload
//! holds them encrypted with the master key (bound to the message ID); the
//! recipient and the message ID stay readable for operators. Without
//! `[smtp] host`, email is off: nothing is queued and the UI hides email
//! features.

use std::time::Duration;

use askama::Template;
use lettre::message::header::{HeaderName, HeaderValue};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use slath_core::config::SmtpTls;
use slath_db::jobs::LeasedJob;
use slath_db::worker::JobError;

use crate::Services;
use crate::error::{Result, ServiceError};

/// The job kind that delivers one email.
pub const EMAIL_SEND: &str = "email.send";

/// One message to send.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Email {
    pub to: String,
    pub subject: String,
    pub text: String,
    /// Optional HTML alternative.
    pub html: Option<String>,
    /// Threading: emails about the same subject share it (`In-Reply-To`,
    /// `References`), e.g. `issue/<id>`.
    pub thread: Option<String>,
    /// `List-Id` value, e.g. `alice/demo.git.example.org`.
    pub list_id: Option<String>,
    /// One-click unsubscribe URL (`List-Unsubscribe`).
    pub unsubscribe: Option<String>,
    /// Send only if this user can still read this repository when the
    /// message goes out (notifications).
    #[serde(default)]
    pub read_check: Option<ReadCheck>,
}

/// A recipient and the repository the message is about.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ReadCheck {
    pub user: uuid::Uuid,
    pub repo: uuid::Uuid,
    /// About a merge request (else an issue).
    #[serde(default)]
    pub merge_request: bool,
}

/// Parameters of every email template.
#[derive(Template)]
#[template(path = "email/test.txt")]
struct TestText<'a> {
    instance: &'a str,
}

#[derive(Template)]
#[template(path = "email/test.html")]
struct TestHtml<'a> {
    instance: &'a str,
}

fn render(t: &impl Template) -> Result<String> {
    t.render().map_err(ServiceError::internal)
}

/// `<thread@host>`: a stable Message-ID for a thread's first message.
fn thread_id(thread: &str, host: &str) -> String {
    let clean: String = thread
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '.'
            }
        })
        .collect();
    format!("<{clean}@{host}>")
}

impl Services {
    /// True if outgoing email is configured.
    #[must_use]
    pub fn email_enabled(&self) -> bool {
        self.config.smtp.enabled()
    }

    fn instance_host(&self) -> String {
        self.config.server.public_url.host().to_owned()
    }

    /// Queues `email` for delivery. Returns `false` (and does nothing) when
    /// email is off.
    pub async fn queue_email(&self, email: &Email) -> Result<bool> {
        if !self.email_enabled() {
            return Ok(false);
        }
        let id = uuid::Uuid::now_v7().to_string();
        let body = serde_json::to_vec(email).map_err(ServiceError::internal)?;
        let sealed = self.seal(&body, "jobs", "payload", &id).await?;
        let payload = serde_json::json!({
            "message_id": id,
            "to": email.to,
            "key_id": sealed.key_id,
            "body": base64_encode(&sealed.ciphertext),
        });
        slath_db::jobs::enqueue(
            &self.pool,
            &slath_db::jobs::NewJob::new("default", EMAIL_SEND, payload)
                .dedupe_key(format!("{EMAIL_SEND}:{id}")),
        )
        .await?;
        Ok(true)
    }

    /// Queues the test message (admin UI and CLI check delivery with it).
    pub async fn queue_test_email(
        &self,
        subject: &slath_core::authz::Subject,
        to: &str,
    ) -> Result<bool> {
        match subject {
            slath_core::authz::Subject::System => {}
            s => self.require_site_admin(s)?,
        }
        let email = self.test_email(to)?;
        self.queue_email(&email).await
    }

    /// The test message to `to`.
    pub fn test_email(&self, to: &str) -> Result<Email> {
        let instance = self.config.server.public_url.origin().to_owned();
        Ok(Email {
            to: to.to_owned(),
            subject: "Slath test message".to_owned(),
            text: render(&TestText {
                instance: &instance,
            })?,
            html: Some(render(&TestHtml {
                instance: &instance,
            })?),
            ..Email::default()
        })
    }

    /// Runs one `email.send` job.
    pub(crate) async fn run_email_job(&self, job: &LeasedJob) -> std::result::Result<(), JobError> {
        let field = |k: &str| {
            job.payload
                .get(k)
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| JobError::Permanent(format!("payload has no {k}")))
        };
        let id = field("message_id")?;
        let ciphertext = base64_decode(field("body")?)
            .ok_or_else(|| JobError::Permanent("payload body is not base64".to_owned()))?;
        let plain = self
            .open(&ciphertext, field("key_id")?, "jobs", "payload", id)
            .map_err(|e| JobError::Permanent(format!("cannot decrypt the message: {e}")))?;
        let email: Email = serde_json::from_slice(plain.expose())
            .map_err(|e| JobError::Permanent(format!("invalid message: {e}")))?;
        if let Some(c) = email.read_check
            && !self
                .user_can_read(
                    slath_core::ids::UserId::from_uuid(c.user),
                    slath_core::ids::RepoId::from_uuid(c.repo),
                    if c.merge_request {
                        "merge_request"
                    } else {
                        "issue"
                    },
                )
                .await
        {
            // Access was lost after the notification was queued.
            return Ok(());
        }
        self.deliver(&email, id).await
    }

    /// Sends one message now (the job handler, and the CLI's test).
    pub async fn deliver(
        &self,
        email: &Email,
        message_id: &str,
    ) -> std::result::Result<(), JobError> {
        let smtp = &self.config.smtp;
        if !smtp.enabled() {
            return Err(JobError::Permanent("email is not configured".to_owned()));
        }
        let message = self
            .build_message(email, message_id)
            .map_err(|e| JobError::Permanent(e.to_string()))?;
        let mut builder = match smtp.tls {
            SmtpTls::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&smtp.host),
            SmtpTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&smtp.host),
            SmtpTls::None => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
                &smtp.host,
            )),
        }
        .map_err(|e| JobError::Permanent(format!("smtp: {e}")))?
        .port(smtp.port)
        .timeout(Some(Duration::from_secs(30)));
        if let Some(file) = &smtp.password_file {
            let password = tokio::fs::read_to_string(file)
                .await
                .map_err(|e| JobError::Retry(format!("cannot read smtp.password_file: {e}")))?;
            builder = builder.credentials(Credentials::new(
                smtp.username.clone(),
                password.trim().to_owned(),
            ));
        }
        let transport = builder.build();
        match transport.send(message).await {
            Ok(_) => Ok(()),
            // 5xx answers are permanent (bad recipient, rejected); everything
            // else (4xx, network, TLS) is retried.
            Err(e) if e.is_permanent() => Err(JobError::Permanent(format!("smtp: {e}"))),
            Err(e) => Err(JobError::Retry(format!("smtp: {e}"))),
        }
    }

    fn build_message(
        &self,
        email: &Email,
        message_id: &str,
    ) -> std::result::Result<Message, String> {
        let from: Mailbox = self
            .config
            .smtp
            .from
            .parse()
            .map_err(|e| format!("smtp.from: {e}"))?;
        let to: Mailbox = email
            .to
            .parse()
            .map_err(|e| format!("invalid recipient: {e}"))?;
        let host = self.instance_host();
        let mut b = Message::builder()
            .from(from)
            .to(to)
            .subject(email.subject.replace(['\r', '\n'], " "))
            .message_id(Some(format!("<{message_id}@{host}>")))
            .header(AutoSubmitted);
        if let Some(t) = &email.thread {
            let root = thread_id(t, &host);
            b = b.in_reply_to(root.clone()).references(root);
        }
        if let Some(l) = &email.list_id {
            b = b.raw(RawHeader::new("List-Id", format!("<{l}>")));
        }
        if let Some(u) = &email.unsubscribe {
            b = b
                .raw(RawHeader::new("List-Unsubscribe", format!("<{u}>")))
                .raw(RawHeader::new(
                    "List-Unsubscribe-Post",
                    "List-Unsubscribe=One-Click".to_owned(),
                ));
        }
        let message = match &email.html {
            Some(html) => b.multipart(MultiPart::alternative_plain_html(
                email.text.clone(),
                html.clone(),
            )),
            None => b.body(email.text.clone()),
        };
        message.map_err(|e| e.to_string())
    }
}

/// `Auto-Submitted: auto-generated` (RFC 3834), so vacation responders stay quiet.
#[derive(Clone)]
struct AutoSubmitted;

impl lettre::message::header::Header for AutoSubmitted {
    fn name() -> HeaderName {
        HeaderName::new_from_ascii_str("Auto-Submitted")
    }
    fn parse(_: &str) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Ok(Self)
    }
    fn display(&self) -> HeaderValue {
        HeaderValue::new(Self::name(), "auto-generated".to_owned())
    }
}

/// A header with a fixed name and value (the values here never contain
/// line breaks: they're built from validated URLs and names).
#[derive(Clone)]
struct RawHeader {
    name: &'static str,
    value: String,
}

impl RawHeader {
    fn new(name: &'static str, value: String) -> Self {
        Self {
            name,
            value: value.replace(['\r', '\n'], ""),
        }
    }
}

impl RawHeader {
    fn into_pair(self) -> (HeaderName, String) {
        (HeaderName::new_from_ascii_str(self.name), self.value)
    }
}

trait RawHeaderExt {
    fn raw(self, h: RawHeader) -> Self;
}

impl RawHeaderExt for lettre::message::MessageBuilder {
    fn raw(self, h: RawHeader) -> Self {
        let (name, value) = h.into_pair();
        self.raw_header(HeaderValue::new(name, value))
    }
}

fn base64_encode(b: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn thread_ids_are_safe() {
        assert_eq!(
            super::thread_id("issue/0190-ab", "git.example.org"),
            "<issue.0190-ab@git.example.org>"
        );
        assert_eq!(super::thread_id("a>b\r\nc", "h"), "<a.b..c@h>");
    }
}
