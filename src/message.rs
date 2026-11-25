//! Email content: a validated template rendered into a pure [`Letter`] value
//! (plain text + HTML). Turning a `Letter` into a wire-format MIME message is
//! a separate, explicit step so that rendering stays deterministic.

use std::time::{SystemTime, UNIX_EPOCH};

use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart, header::ContentType};
use lettre::{Address, Message};

use crate::domain::Participant;
use crate::html::{self, BANNER_CID};
use crate::template::Fill;
pub use crate::template::{DEFAULT_TEMPLATE, Template, TemplateError};

/// A fully rendered email, independent of any transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Letter {
    pub to_name: String,
    pub to: Address,
    pub subject: String,
    pub text: String,
    pub html: String,
}

/// Pure rendering. `receiver_name` is a plain string so callers can pass a
/// redaction marker for previews.
pub fn render(template: &Template, giver: &Participant, receiver_name: &str, year: i32) -> Letter {
    let fill = Fill {
        giver: &giver.name,
        receiver: receiver_name,
        year,
    };
    Letter {
        to_name: giver.name.clone(),
        to: giver.email.clone(),
        subject: template.subject.render_plain(fill),
        text: template.render_body(fill),
        html: html::render(template, fill),
    }
}

/// Inline banner image shown at the top of the HTML email.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    pub bytes: Vec<u8>,
    pub content_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BannerError {
    #[error("banner must be a PNG, JPEG or GIF image")]
    UnsupportedFormat,
    #[error("banner is {0} KB; keep it under {MAX_BANNER_KB} KB so emails stay small")]
    TooLarge(usize),
}

pub const MAX_BANNER_KB: usize = 1024;

impl Banner {
    /// The built-in original artwork (`assets/banner.svg`, rendered to PNG).
    pub fn default_art() -> Self {
        Banner {
            bytes: include_bytes!("../assets/banner.png").to_vec(),
            content_type: "image/png",
        }
    }

    /// The built-in correction artwork (`assets/yeti.svg`, rendered to PNG):
    /// the yeti who fat-fingered the pairing keyboard.
    pub fn correction_art() -> Self {
        Banner {
            bytes: include_bytes!("../assets/yeti.png").to_vec(),
            content_type: "image/png",
        }
    }

    /// Whether this is the normal built-in artwork (which a correction must not reuse).
    pub fn is_default_art(&self) -> bool {
        self.bytes == include_bytes!("../assets/banner.png")
    }

    /// Accept a user-supplied image, detecting its type from magic bytes.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, BannerError> {
        let content_type = match bytes.as_slice() {
            [0x89, b'P', b'N', b'G', ..] => "image/png",
            [0xFF, 0xD8, 0xFF, ..] => "image/jpeg",
            [b'G', b'I', b'F', b'8', ..] => "image/gif",
            _ => return Err(BannerError::UnsupportedFormat),
        };
        let kb = bytes.len().div_ceil(1024);
        if kb > MAX_BANNER_KB {
            return Err(BannerError::TooLarge(kb));
        }
        Ok(Banner {
            bytes,
            content_type,
        })
    }
}

/// Convert to an RFC 5322 message:
///
/// ```text
/// multipart/alternative
/// ├── text/plain
/// └── multipart/related
///     ├── text/html
///     └── image (Content-ID: <banner>, inline)
/// ```
///
/// `message_id: None` generates a fresh one.
pub fn to_message(
    letter: &Letter,
    from: &Mailbox,
    banner: &Banner,
    date: SystemTime,
    message_id: Option<String>,
) -> Result<Message, lettre::error::Error> {
    let image_type = ContentType::parse(banner.content_type).expect("known image content type");
    let html_with_image = MultiPart::related()
        .singlepart(SinglePart::html(letter.html.replace('\n', "\r\n")))
        .singlepart(
            Attachment::new_inline(BANNER_CID.to_owned()).body(banner.bytes.clone(), image_type),
        );
    let body = MultiPart::alternative()
        .singlepart(SinglePart::plain(letter.text.replace('\n', "\r\n")))
        .multipart(html_with_image);

    Message::builder()
        .from(from.clone())
        .to(Mailbox::new(
            Some(letter.to_name.clone()),
            letter.to.clone(),
        ))
        .subject(letter.subject.clone())
        .date(date)
        .message_id(Some(
            message_id.unwrap_or_else(|| generate_message_id(from)),
        ))
        .multipart(body)
}

/// Random Message-ID on the sender's domain. lettre's own generator uses the
/// local hostname, which would leak the organizer's machine name.
fn generate_message_id(from: &Mailbox) -> String {
    use rand::RngExt;
    let token: u128 = rand::rng().random();
    format!("<{token:032x}@{}>", from.email.domain())
}

/// Calendar year (UTC) of a timestamp. Civil-from-days algorithm
/// (H. Hinnant), avoiding a date-time dependency.
pub fn year_of(t: SystemTime) -> i32 {
    let days = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn person(name: &str, email: &str) -> Participant {
        Participant {
            name: name.into(),
            household: "1".into(),
            email: email.parse().unwrap(),
        }
    }

    fn alice_letter() -> Letter {
        render(
            &Template::default_template(),
            &person("Alice Example", "alice@example.com"),
            "Bob",
            2026,
        )
    }

    #[test]
    fn default_template_renders_text() {
        let l = alice_letter();
        assert_eq!(l.subject, "🎄 Your 2026 Family Secret Santa assignment");
        assert!(l.text.starts_with("Ho ho ho, Alice Example! 🎅\n\n"));
        assert!(l.text.contains("🎁 Bob 🎁"));
        assert!(!l.text.contains('{'));
        assert!(!l.html.contains('{'));
    }

    #[test]
    fn text_snapshot() {
        insta::assert_snapshot!(alice_letter().text);
    }

    #[test]
    fn html_snapshot() {
        insta::assert_snapshot!(alice_letter().html);
    }

    #[test]
    fn correction_art_is_a_distinct_small_png() {
        let yeti = Banner::correction_art();
        assert_eq!(
            Banner::from_bytes(yeti.bytes.clone()).unwrap().content_type,
            "image/png"
        );
        assert!(!yeti.is_default_art());
        assert!(Banner::default_art().is_default_art());
    }

    #[test]
    fn banner_detection() {
        assert_eq!(Banner::default_art().content_type, "image/png");
        assert_eq!(
            Banner::from_bytes(vec![0xFF, 0xD8, 0xFF, 0])
                .unwrap()
                .content_type,
            "image/jpeg"
        );
        assert_eq!(
            Banner::from_bytes(b"GIF89a".to_vec()).unwrap().content_type,
            "image/gif"
        );
        assert_eq!(
            Banner::from_bytes(b"<svg>".to_vec()),
            Err(BannerError::UnsupportedFormat)
        );
        let mut huge = vec![0x89, b'P', b'N', b'G'];
        huge.resize((MAX_BANNER_KB + 1) * 1024, 0);
        assert!(matches!(
            Banner::from_bytes(huge),
            Err(BannerError::TooLarge(_))
        ));
    }

    #[test]
    fn year_of_known_dates() {
        let at = |secs: u64| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(year_of(at(0)), 1970);
        assert_eq!(year_of(at(951_782_400)), 2000); // 2000-02-29
        assert_eq!(year_of(at(1_767_225_599)), 2025); // 2025-12-31T23:59:59Z
        assert_eq!(year_of(at(1_767_225_600)), 2026); // 2026-01-01T00:00:00Z
    }

    #[test]
    fn mime_structure_has_text_html_and_inline_banner() {
        let from: Mailbox = "Santa's Elf <elf@example.com>".parse().unwrap();
        let msg = to_message(
            &alice_letter(),
            &from,
            &Banner::default_art(),
            UNIX_EPOCH + Duration::from_secs(1_795_000_000),
            Some("<fixed@example.com>".into()),
        )
        .unwrap();
        let wire = String::from_utf8_lossy(&msg.formatted()).into_owned();
        let headers = &wire[..wire.find("\r\n\r\n").unwrap()];
        for h in [
            "To: \"Alice Example\" <alice@example.com>",
            "Date: Wed, 18 Nov 2026 11:06:40 +0000",
            "Message-ID: <fixed@example.com>",
            "Content-Type: multipart/alternative",
        ] {
            assert!(headers.contains(h), "missing {h:?} in:\n{headers}");
        }
        // Subject with emoji is RFC 2047 encoded.
        assert!(headers.contains("Subject: =?utf-8?"));
        let order: Vec<usize> = [
            "Content-Type: text/plain",
            "Content-Type: multipart/related",
            "Content-Type: text/html",
            "Content-ID: <banner>",
            "Content-Type: image/png",
        ]
        .iter()
        .map(|needle| {
            wire.find(needle)
                .unwrap_or_else(|| panic!("missing {needle}"))
        })
        .collect();
        assert!(order[0] < order[1] && order[1] < order[2], "{order:?}");
        assert!(wire.contains("Content-Disposition: inline"));
    }

    #[test]
    fn generated_message_id_uses_sender_domain() {
        let from: Mailbox = "Elf <elf@example.com>".parse().unwrap();
        let msg = to_message(
            &alice_letter(),
            &from,
            &Banner::default_art(),
            SystemTime::now(),
            None,
        )
        .unwrap();
        let wire = String::from_utf8_lossy(&msg.formatted()).into_owned();
        assert!(wire.contains("@example.com>\r\n"));
        assert!(wire.contains("Message-ID: <"));
    }
}
