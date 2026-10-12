//! 有界 HTML 元素索引：父子关系、属性和文本实体；不执行页面脚本。

use anyhow::{Context, Result, ensure};

const MAX_HTML_BYTES: usize = 8 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 128;

// An intentionally small, bounded index of actual HTML elements. Keeping parent
// relationships avoids selecting a recommendation/comment image as the author;
// skipping raw script/style bodies also excludes HTML-looking JavaScript text.
pub(super) struct Element {
    pub(super) tag: String,
    pub(super) attrs: Vec<(String, String)>,
    pub(super) parent: Option<usize>,
    pub(super) inner_start: usize,
    pub(super) inner_end: usize,
}

impl Element {
    pub(super) fn has_class(&self, name: &str) -> bool {
        self.attr("class")
            .is_some_and(|classes| classes.split_ascii_whitespace().any(|class| class == name))
    }

    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

pub(super) struct Document<'a> {
    pub(super) nodes: Vec<Element>,
    pub(super) text: Vec<(usize, usize, &'a str)>,
}

impl<'a> Document<'a> {
    pub(super) fn parse(html: &'a str) -> Result<Self> {
        ensure!(
            html.len() <= MAX_HTML_BYTES,
            "douyin HTML exceeds metadata limit"
        );
        let mut document = Self {
            nodes: Vec::new(),
            text: Vec::new(),
        };
        let mut stack: Vec<usize> = Vec::new();
        let mut cursor = 0;
        while cursor < html.len() {
            let next = html[cursor..]
                .find('<')
                .map(|offset| cursor + offset)
                .unwrap_or(html.len());
            if next > cursor
                && let Some(&parent) = stack.last()
            {
                document.text.push((parent, cursor, &html[cursor..next]));
            }
            if next == html.len() {
                break;
            }
            if html[next..].starts_with("<!--") {
                cursor = html[next + 4..]
                    .find("-->")
                    .map(|offset| next + 4 + offset + 3)
                    .context("unterminated HTML comment")?;
                continue;
            }
            let end = tag_end(html, next).context("unterminated HTML tag")?;
            cursor = end + 1;
            let raw = html[next + 1..end].trim();
            if raw.starts_with(['!', '?']) {
                continue;
            }
            let closing = raw.starts_with('/');
            let raw = raw.trim_start_matches('/').trim_start();
            let name_end = raw
                .bytes()
                .position(|byte| {
                    !byte.is_ascii_alphanumeric() && !matches!(byte, b'-' | b':' | b'_')
                })
                .unwrap_or(raw.len());
            if name_end == 0 {
                continue;
            }
            let tag = raw[..name_end].to_ascii_lowercase();
            if closing {
                if let Some(position) = stack
                    .iter()
                    .rposition(|&index| document.nodes[index].tag == tag)
                {
                    for index in stack.drain(position..) {
                        document.nodes[index].inner_end = next;
                    }
                }
                continue;
            }
            ensure!(
                document.nodes.len() < MAX_NODES && stack.len() < MAX_DEPTH,
                "douyin HTML structure exceeds metadata limit"
            );
            let index = document.nodes.len();
            document.nodes.push(Element {
                tag: tag.clone(),
                attrs: attributes(&raw[name_end..]),
                parent: stack.last().copied(),
                inner_start: cursor,
                inner_end: cursor,
            });
            if matches!(tag.as_str(), "script" | "style" | "textarea" | "title") {
                // The HTML raw-text terminator is case insensitive. Do not run
                // or recursively parse anything between its opening/closing tags.
                let terminator = format!("</{tag}");
                let stop = html[cursor..]
                    .as_bytes()
                    .windows(terminator.len())
                    .position(|bytes| bytes.eq_ignore_ascii_case(terminator.as_bytes()))
                    .map(|offset| cursor + offset)
                    .context("unterminated HTML raw text")?;
                document.nodes[index].inner_end = stop;
                cursor = tag_end(html, stop).context("unterminated raw text closing tag")? + 1;
            } else if matches!(
                tag.as_str(),
                "area"
                    | "base"
                    | "br"
                    | "col"
                    | "embed"
                    | "hr"
                    | "img"
                    | "input"
                    | "link"
                    | "meta"
                    | "param"
                    | "source"
                    | "track"
                    | "wbr"
            ) || raw.ends_with('/')
            {
                if tag == "br" {
                    document.text.push((index, cursor, "\n"));
                }
            } else {
                stack.push(index);
            }
        }
        for index in stack {
            document.nodes[index].inner_end = html.len();
        }
        Ok(document)
    }

    pub(super) fn within(&self, index: usize, parent: usize) -> bool {
        let mut current = Some(index);
        while let Some(index) = current {
            if index == parent {
                return true;
            }
            current = self.nodes[index].parent;
        }
        false
    }

    pub(super) fn ancestor(
        &self,
        index: usize,
        predicate: impl Fn(&Element) -> bool,
    ) -> Option<usize> {
        let mut current = self.nodes[index].parent;
        while let Some(index) = current {
            if predicate(&self.nodes[index]) {
                return Some(index);
            }
            current = self.nodes[index].parent;
        }
        None
    }

    pub(super) fn descendants(&self, parent: usize) -> impl Iterator<Item = usize> + '_ {
        (parent + 1..self.nodes.len()).filter(move |&index| self.within(index, parent))
    }

    pub(super) fn text(&self, parent: usize) -> String {
        let text = self
            .text
            .iter()
            .filter(|(index, _, _)| self.within(*index, parent))
            .map(|(_, _, text)| *text)
            .collect::<String>();
        decode_entities(&text)
    }

    pub(super) fn rich_text(&self, parent: usize) -> String {
        let mut pieces = self
            .text
            .iter()
            .filter(|(index, _, _)| self.within(*index, parent))
            .map(|(_, offset, text)| (*offset, decode_entities(text)))
            .collect::<Vec<_>>();
        for index in self.descendants(parent) {
            let node = &self.nodes[index];
            if node.tag == "img"
                && let Some(alt) = node.attr("alt")
            {
                pieces.push((node.inner_start, alt.to_owned()));
            }
        }
        pieces.sort_by_key(|(offset, _)| *offset);
        pieces.into_iter().map(|(_, text)| text).collect()
    }

    pub(super) fn meta(&self, name: &str) -> Option<&str> {
        self.nodes
            .iter()
            .find(|node| node.tag == "meta" && node.attr("name") == Some(name))?
            .attr("content")
    }
}

fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, byte) in html.as_bytes()[start + 1..].iter().copied().enumerate() {
        match (quote, byte) {
            (Some(delimiter), byte) if delimiter == byte => quote = None,
            (None, b'\'' | b'"') => quote = Some(byte),
            (None, b'>') => return Some(start + 1 + offset),
            _ => {}
        }
    }
    None
}

fn attributes(raw: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    let mut rest = raw.trim_start();
    while !rest.is_empty() && rest != "/" {
        let name_end = rest
            .find(|character: char| {
                character.is_ascii_whitespace() || matches!(character, '=' | '/' | '>')
            })
            .unwrap_or(rest.len());
        if name_end == 0 {
            break;
        }
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let value = if let Some(after) = rest.strip_prefix('=') {
            rest = after.trim_start();
            if let Some(quote) = rest
                .chars()
                .next()
                .filter(|character| matches!(character, '\'' | '"'))
            {
                rest = &rest[1..];
                let end = rest.find(quote).unwrap_or(rest.len());
                let value = &rest[..end];
                rest = rest.get(end + 1..).unwrap_or("").trim_start();
                value
            } else {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let value = &rest[..end];
                rest = rest[end..].trim_start();
                value
            }
        } else {
            ""
        };
        if !result.iter().any(|(existing, _)| existing == &name) {
            result.push((name, decode_entities(value)));
        }
    }
    result
}

/// Decode once, so `&amp;lt;` remains literal `&lt;` instead of becoming markup.
pub(super) fn decode_entities(raw: &str) -> String {
    let mut result = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';').filter(|&end| end <= 32) else {
            result.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "nbsp" => Some('\u{a0}'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|number| u32::from_str_radix(number, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|number| number.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        if let Some(character) = decoded {
            result.push(character);
            rest = &rest[end + 1..];
        } else {
            result.push('&');
            rest = &rest[1..];
        }
    }
    result.push_str(rest);
    result
}
