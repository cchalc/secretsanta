//! Email templates, parsed once into segments and rendered many times.
//!
//! A template is plain text with `{giver}`, `{receiver}` and `{year}`
//! placeholders:
//!
//! ```text
//! Subject: ...
//!
//! paragraph
//!
//! paragraph
//! ```
//!
//! Parsing up front means placeholders are validated once, and rendering is a
//! single pass: substituted values are never re-scanned, so a name that happens
//! to contain `{receiver}` stays literal.

pub const DEFAULT_TEMPLATE: &str = "\
Subject: 🎄 Your {year} Family Secret Santa assignment

Ho ho ho, {giver}! 🎅

The elves have drawn names for the {year} Family Secret Santa, and you are the Secret Santa for…

🎁 {receiver} 🎁

Shh… it's a secret! 🤫 Please don't tell anyone who you have.

Need ideas? Share your own wish list in the WhatsApp family chat so your Secret Santa knows what you'd love.

Merry Christmas and happy holidays!
Santa's Elf 🧝
";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("template must start with a `Subject: ...` line followed by a blank line")]
    MissingSubject,
    #[error(
        "template subject must not contain {{receiver}} (subjects are shown in previews and inbox lists)"
    )]
    ReceiverInSubject,
    #[error(
        "template body must contain {{receiver}}, otherwise the giver never learns who to buy for"
    )]
    MissingReceiver,
    #[error("unknown placeholder {{{0}}}; allowed: {{giver}}, {{receiver}}, {{year}}")]
    UnknownPlaceholder(String),
    #[error("unclosed '{{' in template")]
    Unclosed,
}

/// Values substituted into a template.
#[derive(Clone, Copy, Debug)]
pub struct Fill<'a> {
    pub giver: &'a str,
    pub receiver: &'a str,
    pub year: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Giver,
    Receiver,
    Year,
}

/// A run of template text: literals interleaved with placeholders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text(Vec<Segment>);

impl Text {
    fn parse(s: &str) -> Result<Self, TemplateError> {
        let mut chunks = s.split('{');
        let head = Segment::Literal(chunks.next().unwrap_or_default().to_owned());
        let rest = chunks
            .map(|chunk| {
                let (name, after) = chunk.split_once('}').ok_or(TemplateError::Unclosed)?;
                let placeholder = match name {
                    "giver" => Segment::Giver,
                    "receiver" => Segment::Receiver,
                    "year" => Segment::Year,
                    other => return Err(TemplateError::UnknownPlaceholder(other.to_owned())),
                };
                Ok([placeholder, Segment::Literal(after.to_owned())])
            })
            .collect::<Result<Vec<_>, _>>()?;
        let segments = std::iter::once(head)
            .chain(rest.into_iter().flatten())
            .filter(|s| *s != Segment::Literal(String::new()))
            .collect();
        Ok(Text(segments))
    }

    pub fn mentions_receiver(&self) -> bool {
        self.0.contains(&Segment::Receiver)
    }

    /// Single-pass render. `escape` is applied to literals and values (identity
    /// for plain text, HTML-escaping for HTML); `wrap_receiver` decorates the
    /// already-escaped receiver.
    pub fn render_with(
        &self,
        fill: Fill,
        escape: impl Fn(&str) -> String,
        wrap_receiver: impl Fn(String) -> String,
    ) -> String {
        self.0
            .iter()
            .map(|segment| match segment {
                Segment::Literal(text) => escape(text),
                Segment::Giver => escape(fill.giver),
                Segment::Receiver => wrap_receiver(escape(fill.receiver)),
                Segment::Year => fill.year.to_string(),
            })
            .collect()
    }

    pub fn render_plain(&self, fill: Fill) -> String {
        self.render_with(fill, str::to_owned, |receiver| receiver)
    }
}

/// A validated email template: a subject and body paragraphs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub subject: Text,
    pub paragraphs: Vec<Text>,
}

impl Template {
    pub fn parse(text: &str) -> Result<Self, TemplateError> {
        let text = text.replace("\r\n", "\n");
        let (first, rest) = text.split_once('\n').ok_or(TemplateError::MissingSubject)?;
        let subject = first
            .strip_prefix("Subject:")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(TemplateError::MissingSubject)?;
        let body = rest
            .strip_prefix('\n')
            .ok_or(TemplateError::MissingSubject)?;

        let subject = Text::parse(subject)?;
        let paragraphs = body
            .split("\n\n")
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(Text::parse)
            .collect::<Result<Vec<_>, _>>()?;

        if subject.mentions_receiver() {
            return Err(TemplateError::ReceiverInSubject);
        }
        if !paragraphs.iter().any(Text::mentions_receiver) {
            return Err(TemplateError::MissingReceiver);
        }
        Ok(Template {
            subject,
            paragraphs,
        })
    }

    pub fn default_template() -> Self {
        Template::parse(DEFAULT_TEMPLATE).expect("built-in template is valid")
    }

    /// Plain-text body: paragraphs separated by blank lines.
    pub fn render_body(&self, fill: Fill) -> String {
        let paragraphs: Vec<String> = self
            .paragraphs
            .iter()
            .map(|p| p.render_plain(fill))
            .collect();
        paragraphs.join("\n\n") + "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILL: Fill = Fill {
        giver: "Al",
        receiver: "Bo",
        year: 2026,
    };

    #[test]
    fn validation() {
        assert_eq!(
            Template::parse("no subject\n\nbody {receiver}"),
            Err(TemplateError::MissingSubject)
        );
        assert_eq!(
            Template::parse("Subject: x\nbody {receiver}"),
            Err(TemplateError::MissingSubject)
        );
        assert_eq!(
            Template::parse("Subject: for {receiver}\n\nbody {receiver}"),
            Err(TemplateError::ReceiverInSubject)
        );
        assert_eq!(
            Template::parse("Subject: x\n\nbody"),
            Err(TemplateError::MissingReceiver)
        );
        assert_eq!(
            Template::parse("Subject: x\n\n{reciever} typo"),
            Err(TemplateError::UnknownPlaceholder("reciever".into()))
        );
        assert_eq!(
            Template::parse("Subject: x\n\n{receiver"),
            Err(TemplateError::Unclosed)
        );
        assert!(
            Template::parse("Subject: {year}\r\n\r\nHi {giver}, buy for {receiver}\r\n").is_ok()
        );
    }

    #[test]
    fn renders_in_a_single_pass() {
        let t = Template::parse("Subject: {year}\n\n{giver} → {receiver} ({year})").unwrap();
        assert_eq!(t.subject.render_plain(FILL), "2026");
        assert_eq!(t.render_body(FILL), "Al → Bo (2026)\n");
        // Values are never re-scanned for placeholders.
        let sneaky = Fill {
            giver: "{receiver}",
            ..FILL
        };
        assert_eq!(t.render_body(sneaky), "{receiver} → Bo (2026)\n");
    }

    #[test]
    fn escape_and_wrap_apply_per_segment() {
        let t = Text::parse("<{giver}> gives to {receiver}").unwrap();
        let html = t.render_with(
            Fill {
                giver: "A&B",
                ..FILL
            },
            |s| s.replace('&', "&amp;"),
            |r| format!("[{r}]"),
        );
        assert_eq!(html, "<A&amp;B> gives to [Bo]");
    }
}
