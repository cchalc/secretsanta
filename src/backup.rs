//! Encrypted history backups that are safe to commit to a public repo.
//!
//! History is encrypted with [age](https://age-encryption.org) to the public
//! keys in `history/recipients.txt`. Encrypting needs no secret, so a backup
//! can be written automatically every time history changes. Only restoring
//! needs the organizer's secret key, which never enters the repo.
//!
//! Pure functions over bytes and text; the shell does the file I/O.

use std::io::Write;

use age::x25519::{Identity, Recipient};

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("recipients file has no age public key (a line starting with `age1`)")]
    NoRecipients,
    #[error("recipients file line {line}: not a valid age public key")]
    BadRecipient { line: usize },
    #[error("identity file has no age secret key (a line starting with `AGE-SECRET-KEY-`)")]
    NoIdentity,
    #[error("identity file contains an invalid age secret key")]
    BadIdentity,
    #[error("cannot encrypt history: {0}")]
    Encrypt(String),
    #[error("cannot decrypt the history backup: wrong key, or the file is damaged ({0})")]
    Decrypt(String),
}

/// Public keys, one per line. Blank lines and `#` comments are ignored.
pub fn parse_recipients(text: &str) -> Result<Vec<Recipient>, BackupError> {
    let recipients = text
        .lines()
        .enumerate()
        .map(|(i, line)| (i + 1, line.trim()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(line, key)| {
            key.parse::<Recipient>()
                .map_err(|_| BackupError::BadRecipient { line })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if recipients.is_empty() {
        return Err(BackupError::NoRecipients);
    }
    Ok(recipients)
}

/// The secret key from an `age-keygen` identity file (comments are ignored).
pub fn parse_identity(text: &str) -> Result<Identity, BackupError> {
    let key = text
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("AGE-SECRET-KEY-"))
        .ok_or(BackupError::NoIdentity)?;
    key.parse::<Identity>()
        .map_err(|_| BackupError::BadIdentity)
}

/// ASCII-armored ciphertext of `plaintext`, readable by any of `recipients`.
pub fn encrypt(recipients: &[Recipient], plaintext: &[u8]) -> Result<String, BackupError> {
    let fail = |e: &dyn std::fmt::Display| BackupError::Encrypt(e.to_string());
    let encryptor =
        age::Encryptor::with_recipients(recipients.iter().map(|r| r as &dyn age::Recipient))
            .map_err(|e| fail(&e))?;
    let mut armored = Vec::new();
    let mut writer = encryptor
        .wrap_output(
            age::armor::ArmoredWriter::wrap_output(&mut armored, age::armor::Format::AsciiArmor)
                .map_err(|e| fail(&e))?,
        )
        .map_err(|e| fail(&e))?;
    writer.write_all(plaintext).map_err(|e| fail(&e))?;
    writer
        .finish()
        .and_then(|armor| armor.finish())
        .map_err(|e| fail(&e))?;
    Ok(String::from_utf8(armored).expect("armored output is ASCII"))
}

pub fn decrypt(identity: &Identity, ciphertext: &[u8]) -> Result<Vec<u8>, BackupError> {
    age::decrypt(identity, ciphertext).map_err(|e| BackupError::Decrypt(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use age::secrecy::ExposeSecret;

    fn keypair() -> (Identity, String) {
        let identity = Identity::generate();
        let public = identity.to_public().to_string();
        (identity, public)
    }

    #[test]
    fn round_trip_and_no_plaintext_in_ciphertext() {
        let (identity, public) = keypair();
        let recipients = parse_recipients(&format!("# organizer\n{public}\n")).unwrap();
        let secret = br#"{"giver":"alice@example.com","receiver":"bob@example.com"}"#;
        let armored = encrypt(&recipients, secret).unwrap();
        assert!(armored.starts_with("-----BEGIN AGE ENCRYPTED FILE-----"));
        assert!(!armored.contains("alice") && !armored.contains("example.com"));
        assert_eq!(decrypt(&identity, armored.as_bytes()).unwrap(), secret);
    }

    #[test]
    fn identity_file_comments_are_ignored() {
        let (identity, public) = keypair();
        let file = format!(
            "# created: 2026-11-24\n# public key: {public}\n{}\n",
            identity.to_string().expose_secret()
        );
        let parsed = parse_identity(&file).unwrap();
        assert_eq!(parsed.to_public().to_string(), public);
    }

    #[test]
    fn wrong_key_and_bad_input_are_errors() {
        let (_, public) = keypair();
        let (other, _) = keypair();
        let armored = encrypt(&parse_recipients(&public).unwrap(), b"history").unwrap();
        assert!(matches!(
            decrypt(&other, armored.as_bytes()),
            Err(BackupError::Decrypt(_))
        ));
        assert!(matches!(
            parse_recipients("# none\n\n"),
            Err(BackupError::NoRecipients)
        ));
        assert!(matches!(
            parse_recipients("age1notakey"),
            Err(BackupError::BadRecipient { line: 1 })
        ));
        assert!(matches!(
            parse_identity("nothing here"),
            Err(BackupError::NoIdentity)
        ));
    }
}
