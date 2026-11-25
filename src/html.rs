//! Festive HTML layout for the email.
//!
//! Email clients are not browsers: Gmail and Outlook strip most `<style>`
//! blocks and modern CSS, so the layout uses nested tables and inline styles
//! only. Every piece of user data is HTML-escaped.
//!
//! Template paragraphs become `<p>` elements, except the paragraph that
//! mentions the receiver, which becomes a highlighted "reveal" card.

use crate::template::{Fill, Template, Text};

/// Content-ID of the inline banner image.
pub const BANNER_CID: &str = "banner";

/// Hidden inbox-preview text. Must never contain the receiver.
pub const PREHEADER: &str =
    "The elves have drawn names. Your Secret Santa assignment is inside! 🎁";

const PINE: &str = "#14532d";
const BERRY: &str = "#c62828";
const GOLD: &str = "#f9c74f";
const SNOW: &str = "#fffaf2";
const INK: &str = "#2b2b2b";
const MUTED: &str = "#7a6f63";
const SERIF: &str = "Georgia,'Times New Roman',serif";
const SANS: &str = "'Helvetica Neue',Helvetica,Arial,sans-serif";

pub fn escape(s: &str) -> String {
    s.chars()
        .fold(String::with_capacity(s.len()), |mut out, c| {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&#39;"),
                c => out.push(c),
            }
            out
        })
}

/// Render a paragraph: escaped, line breaks kept, receiver wrapped.
fn paragraph_html(p: &Text, fill: Fill, wrap_receiver: impl Fn(String) -> String) -> String {
    p.render_with(fill, escape, wrap_receiver)
        .replace('\n', "<br>")
}

fn plain_paragraph(p: &Text, fill: Fill) -> String {
    let content = paragraph_html(p, fill, |r| format!("<strong>{r}</strong>"));
    format!(r#"<p style="margin:0 0 16px;">{content}</p>"#)
}

fn reveal_card(p: &Text, fill: Fill) -> String {
    let content = paragraph_html(p, fill, |r| {
        format!(
            r#"<span style="display:inline-block;font-size:30px;line-height:1.2;font-weight:bold;color:#ffffff;padding:4px 8px;">{r}</span>"#
        )
    });
    format!(
        r#"<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" style="margin:4px 0 22px;">
<tr><td align="center" bgcolor="{BERRY}" style="background-color:{BERRY};border:3px dashed {GOLD};border-radius:14px;padding:20px 12px;font-family:{SERIF};font-size:24px;line-height:1.4;color:#ffffff;">{content}</td></tr>
</table>"#
    )
}

/// Render the full HTML document.
pub fn render(template: &Template, fill: Fill) -> String {
    let content = template
        .paragraphs
        .iter()
        .map(|p| {
            if p.mentions_receiver() {
                reveal_card(p, fill)
            } else {
                plain_paragraph(p, fill)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let title = template.subject.render_with(fill, escape, |r| r);
    let year = fill.year;

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light">
<title>{title}</title>
</head>
<body style="margin:0;padding:0;background-color:{PINE};">
<div style="display:none;max-height:0;overflow:hidden;opacity:0;color:{PINE};">{preheader}</div>
<table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" bgcolor="{PINE}" style="background-color:{PINE};">
<tr><td align="center" style="padding:28px 12px;">
  <!--[if mso]><table role="presentation" width="600" align="center" cellpadding="0" cellspacing="0" border="0"><tr><td><![endif]-->
  <table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0" bgcolor="{SNOW}" style="width:100%;max-width:600px;background-color:{SNOW};border-radius:16px;overflow:hidden;">
    <tr><td style="padding:0;line-height:0;">
      <img src="cid:{BANNER_CID}" width="600" alt="Secret Santa holiday banner" style="display:block;width:100%;max-width:600px;height:auto;border:0;">
    </td></tr>
    <tr><td align="center" style="padding:26px 32px 4px;font-family:{SERIF};font-size:30px;line-height:1.2;font-weight:bold;color:{BERRY};">
      Secret Santa {year}
    </td></tr>
    <tr><td align="center" style="padding:0 32px 18px;font-family:{SANS};font-size:13px;letter-spacing:3px;text-transform:uppercase;color:{PINE};">
      &#10052; &nbsp;a little holiday magic&nbsp; &#10052;
    </td></tr>
    <tr><td style="padding:8px 28px 12px;font-family:{SANS};font-size:17px;line-height:1.6;color:{INK};">
{content}
    </td></tr>
    <tr><td style="padding:0 28px;"><table role="presentation" width="100%" cellpadding="0" cellspacing="0" border="0"><tr><td style="border-top:2px dotted {GOLD};font-size:0;line-height:0;">&nbsp;</td></tr></table></td></tr>
    <tr><td align="center" style="padding:16px 28px 30px;font-family:{SANS};font-size:13px;line-height:1.5;color:{MUTED};">
      &#129323; This message is just for you. Please keep it a secret!
    </td></tr>
  </table>
  <!--[if mso]></td></tr></table><![endif]-->
</td></tr>
</table>
</body>
</html>
"#,
        preheader = escape(PREHEADER),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn html(template: &str, giver: &str, receiver: &str) -> String {
        let t = Template::parse(&format!("Subject: {{year}} Secret Santa\n\n{template}")).unwrap();
        render(
            &t,
            Fill {
                giver,
                receiver,
                year: 2026,
            },
        )
    }

    #[test]
    fn escapes_user_data() {
        let out = html("Hi {giver}\n\nFor {receiver}", "<b>Al</b>", "Bo & \"Co\"");
        assert!(out.contains("Hi &lt;b&gt;Al&lt;/b&gt;"));
        assert!(out.contains("Bo &amp; &quot;Co&quot;"));
        assert!(!out.contains("<b>Al</b>"));
    }

    #[test]
    fn escapes_template_text_too() {
        let out = html("Tom & Jerry say <hi>\n\n{receiver}", "A", "B");
        assert!(out.contains("Tom &amp; Jerry say &lt;hi&gt;"));
    }

    #[test]
    fn receiver_paragraph_becomes_a_reveal_card() {
        let out = html("Hello\n\n🎁 {receiver} 🎁\n\nBye", "A", "Bob");
        let card = out.find("border:3px dashed").unwrap();
        let name = out.find(">Bob</span>").unwrap();
        assert!(card < name);
        assert_eq!(out.matches("<p ").count(), 2);
    }

    #[test]
    fn preheader_and_title_never_contain_the_receiver() {
        let out = html("{receiver}", "A", "Zelda");
        let head = &out[..out.find("<table").unwrap()];
        assert!(!head.contains("Zelda"));
        assert!(head.contains("2026 Secret Santa"));
    }

    #[test]
    fn references_the_inline_banner() {
        assert!(html("{receiver}", "A", "B").contains("src=\"cid:banner\""));
    }
}
