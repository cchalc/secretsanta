//! Delivery is an effect, so it sits behind a trait. The send pipeline only
//! sees `&dyn Mailer`; production uses SMTP, rehearsals write `.eml` files,
//! and tests record letters in memory.

use std::cell::Cell;
#[cfg(test)]
use std::cell::RefCell;
use std::path::PathBuf;
use std::time::SystemTime;

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{SmtpTransport, Transport};

use crate::files::{ensure_private_dir, write_private};
use crate::message::{Banner, Letter, to_message};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MailError {
    #[error("mail configuration error: {0}")]
    Config(String),
    #[error("could not build message: {0}")]
    Build(String),
    #[error("delivery failed: {0}")]
    Transport(String),
}

pub trait Mailer {
    /// Check connectivity/credentials before anything is sent.
    fn preflight(&self) -> Result<(), MailError> {
        Ok(())
    }
    fn deliver(&self, letter: &Letter) -> Result<(), MailError>;
}

/// How the SMTP connection is secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Security {
    /// Upgrade a plain connection with STARTTLS (Gmail port 587).
    Starttls,
    /// Implicit TLS from the first byte (port 465).
    Tls,
    /// No encryption and no login. For a local test inbox such as Mailpit only.
    None,
}

impl Security {
    pub fn default_port(self) -> u16 {
        match self {
            Security::Starttls => 587,
            Security::Tls => 465,
            Security::None => 1025,
        }
    }
}

/// Everything needed to talk to an SMTP server.
#[derive(Clone, Debug)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    pub security: Security,
    /// `(username, password)`; never used with `Security::None`.
    pub credentials: Option<(String, String)>,
}

/// SMTP with a pooled connection (one login for the whole run).
pub struct SmtpMailer {
    transport: SmtpTransport,
    from: Mailbox,
    banner: Banner,
}

impl SmtpMailer {
    pub fn new(config: &SmtpConfig, from: Mailbox, banner: Banner) -> Result<Self, MailError> {
        let config_err = |e: lettre::transport::smtp::Error| MailError::Config(e.to_string());
        let builder = match config.security {
            Security::Starttls => {
                SmtpTransport::starttls_relay(&config.host).map_err(config_err)?
            }
            Security::Tls => SmtpTransport::relay(&config.host).map_err(config_err)?,
            Security::None => SmtpTransport::builder_dangerous(&config.host),
        }
        .port(config.port);
        let builder = match (&config.credentials, config.security) {
            (Some(_), Security::None) => {
                return Err(MailError::Config(
                    "refusing to send credentials over an unencrypted connection".into(),
                ));
            }
            (Some((user, pass)), Security::Starttls | Security::Tls) => {
                builder.credentials(Credentials::new(user.clone(), pass.clone()))
            }
            (None, _) => builder,
        };
        Ok(SmtpMailer {
            transport: builder.build(),
            from,
            banner,
        })
    }
}

impl Mailer for SmtpMailer {
    fn preflight(&self) -> Result<(), MailError> {
        match self.transport.test_connection() {
            Ok(true) => Ok(()),
            Ok(false) => Err(MailError::Transport(
                "SMTP server did not accept the connection".into(),
            )),
            Err(e) => Err(MailError::Transport(e.to_string())),
        }
    }

    fn deliver(&self, letter: &Letter) -> Result<(), MailError> {
        let msg = to_message(letter, &self.from, &self.banner, SystemTime::now(), None)
            .map_err(|e| MailError::Build(e.to_string()))?;
        self.transport
            .send(&msg)
            .map(|_| ())
            .map_err(|e| MailError::Transport(e.to_string()))
    }
}

/// Writes each message as an `.eml` file. For rehearsals with a test roster:
/// the files contain the full pairing, so never point this at the real list
/// unless you intend to see it.
pub struct OutboxMailer {
    dir: PathBuf,
    from: Mailbox,
    banner: Banner,
    counter: Cell<usize>,
}

impl OutboxMailer {
    pub fn new(dir: PathBuf, from: Mailbox, banner: Banner) -> Self {
        OutboxMailer {
            dir,
            from,
            banner,
            counter: Cell::new(0),
        }
    }
}

impl Mailer for OutboxMailer {
    fn preflight(&self) -> Result<(), MailError> {
        ensure_private_dir(&self.dir).map_err(|e| MailError::Config(e.to_string()))
    }

    fn deliver(&self, letter: &Letter) -> Result<(), MailError> {
        let msg = to_message(letter, &self.from, &self.banner, SystemTime::now(), None)
            .map_err(|e| MailError::Build(e.to_string()))?;
        let n = self.counter.get() + 1;
        self.counter.set(n);
        let path = self.dir.join(format!("{n:03}-{}.eml", letter.to));
        write_private(&path, &msg.formatted()).map_err(|e| MailError::Transport(e.to_string()))
    }
}

/// In-memory mailer for tests. Optionally fails on the k-th delivery
/// (1-based) to simulate a mid-run outage.
#[cfg(test)]
#[derive(Default)]
pub struct RecordingMailer {
    pub sent: RefCell<Vec<Letter>>,
    pub fail_on: Option<usize>,
    attempts: Cell<usize>,
}

#[cfg(test)]
impl RecordingMailer {
    pub fn failing_on(k: usize) -> Self {
        RecordingMailer {
            fail_on: Some(k),
            ..Default::default()
        }
    }
}

#[cfg(test)]
impl Mailer for RecordingMailer {
    fn deliver(&self, letter: &Letter) -> Result<(), MailError> {
        let attempt = self.attempts.get() + 1;
        self.attempts.set(attempt);
        if self.fail_on == Some(attempt) {
            return Err(MailError::Transport("simulated outage".into()));
        }
        self.sent.borrow_mut().push(letter.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_are_refused_without_tls() {
        let config = SmtpConfig {
            host: "127.0.0.1".into(),
            port: 1025,
            security: Security::None,
            credentials: Some(("me@example.com".into(), "secret".into())),
        };
        let from: Mailbox = "Elf <elf@example.com>".parse().unwrap();
        let err = SmtpMailer::new(&config, from, Banner::default_art())
            .err()
            .unwrap();
        assert!(err.to_string().contains("unencrypted"));
    }
}
