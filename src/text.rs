//! Text conversions used by `paste`.

use pulldown_cmark::{Options, Parser, html};

/// Renders Markdown as HTML.
pub fn markdown_to_html(markdown: &str) -> String {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut out = String::new();
    html::push_html(&mut out, Parser::new_ext(markdown, options));
    out
}

/// Extracts readable plain text from HTML for targets that cannot accept
/// rich text.
pub fn html_to_text(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..start]));
        let Some(end) = rest[start..].find('>') else {
            rest = &rest[start..];
            break;
        };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if !tag.starts_with('/') && (name == "script" || name == "style") {
            let close = format!("</{name}");
            let lower = rest.to_ascii_lowercase();
            rest = match lower.find(&close) {
                Some(i) => &rest[i..],
                None => "",
            };
            continue;
        }
        match name.as_str() {
            "br" => out.push('\n'),
            "li" if !tag.starts_with('/') => out.push_str("\n- "),
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "table"
            | "tr" | "blockquote" | "pre" => out.push('\n'),
            "td" | "th" if tag.starts_with('/') => out.push('\t'),
            _ => {}
        }
    }
    out.push_str(&decode_entities(rest));

    let mut collapsed = String::new();
    let mut newlines = 0;
    for line in out.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            newlines += 1;
            continue;
        }
        if !collapsed.is_empty() {
            collapsed.push_str(if newlines > 0 { "\n\n" } else { "\n" });
        }
        newlines = 0;
        collapsed.push_str(line);
    }
    collapsed
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let entity = &rest[1..semi];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| entity.strip_prefix('#').map(|dec| dec.parse::<u32>()))
                    .and_then(|n| n.ok())
                    .and_then(char::from_u32),
            }?;
            Some((c, semi))
        });
        match decoded {
            Some((c, semi)) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_renders_html() {
        let html = markdown_to_html("# Greendale\n\n**Troy** and *Abed*");
        assert!(html.contains("<h1>Greendale</h1>"), "{html}");
        assert!(html.contains("<strong>Troy</strong>"), "{html}");
        assert!(html.contains("<em>Abed</em>"), "{html}");
    }

    #[test]
    fn html_to_text_cases() {
        let cases = [
            ("plain", "Troy Barnes", "Troy Barnes"),
            (
                "entities",
                "Troy &amp; Abed &lt;3 &#233;&#x21;",
                "Troy & Abed <3 é!",
            ),
            ("paragraphs", "<p>One</p><p>Two</p>", "One\n\nTwo"),
            ("line break", "One<br>Two", "One\nTwo"),
            (
                "list",
                "<ul><li>Troy</li><li>Abed</li></ul>",
                "- Troy\n- Abed",
            ),
            (
                "style removed",
                "<style>p{color:red}</style><p>Hi</p>",
                "Hi",
            ),
            ("stray ampersand", "Fish & chips", "Fish & chips"),
            ("blank lines collapse", "<p>A</p>\n\n\n<p>B</p>", "A\n\nB"),
        ];
        for (name, input, want) in cases {
            assert_eq!(html_to_text(input), want, "{name}");
        }
    }
}
