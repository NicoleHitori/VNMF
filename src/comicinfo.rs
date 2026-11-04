// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! ComicInfo.xml ↔ JSON conversion.
//!
//! When packing a CBZ, ComicInfo.xml metadata is converted to JSON and stored in the Info section,
//! and when unpacking to CBZ — the reverse. Convention (lossless for ComicInfo data):
//!
//! - the root is stored explicitly: `{"ComicInfo": {...}}`;
//! - an element with only text → a string: `"Title": "..."`;
//! - attributes → keys with `@`: `"@Image": "0"`;
//! - text next to attributes or child elements → `"#text"`;
//! - repeated elements → an array: `"Page": [{...}, {...}]`;
//! - all values stay strings (no type guessing), so XML → JSON → XML preserves the data.
//!   XML comments and formatting are not preserved.

use anyhow::{Context, Result, bail};
use quick_xml::escape::escape;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use serde_json::{Map, Value};
use vnmf_core::PageRole;

pub const ROOT: &str = "ComicInfo";
pub const FILE_NAME: &str = "ComicInfo.xml";

#[derive(Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn from_start(e: &BytesStart<'_>) -> Result<Self> {
        let name = e.name().as_ref().to_string();
        let mut attrs = Vec::new();
        for attr in e.attributes() {
            let attr = attr?;
            let key = attr.key.as_ref().to_string();
            attrs.push((key, attr.normalized_value(XmlVersion::Implicit1_0)?.into_owned()));
        }
        Ok(Self { name, attrs, ..Default::default() })
    }

    fn into_json(self) -> Value {
        if self.attrs.is_empty() && self.children.is_empty() {
            return Value::String(self.text);
        }

        let mut obj = Map::new();
        for (k, v) in self.attrs {
            obj.insert(format!("@{k}"), Value::String(v));
        }
        // Whitespace between child elements (indentation) is not data.
        let text = self.text.trim();
        if !text.is_empty() {
            obj.insert("#text".into(), Value::String(text.to_string()));
        }
        for child in self.children {
            let name = child.name.clone();
            let value = child.into_json();
            match obj.get_mut(&name) {
                Some(Value::Array(items)) => items.push(value),
                Some(existing) => {
                    let first = existing.take();
                    *existing = Value::Array(vec![first, value]);
                }
                None => {
                    obj.insert(name, value);
                }
            }
        }
        Value::Object(obj)
    }
}

/// ComicInfo.xml (or any XML) → JSON using this module's convention.
pub fn xml_to_json(xml: &str) -> Result<Value> {
    let mut reader = Reader::from_str(xml.trim_start_matches('\u{feff}'));
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;

    let attach = |node: Node, stack: &mut Vec<Node>, root: &mut Option<Node>| -> Result<()> {
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None if root.is_none() => *root = Some(node),
            None => bail!("XML has more than one root element"),
        }
        Ok(())
    };

    loop {
        let pos = reader.buffer_position();
        let event = reader.read_event().with_context(|| format!("XML error near byte {pos}"))?;
        match event {
            Event::Start(e) => stack.push(Node::from_start(&e)?),
            Event::Empty(e) => attach(Node::from_start(&e)?, &mut stack, &mut root)?,
            Event::End(_) => {
                let node = stack.pop().context("unexpected closing tag")?;
                attach(node, &mut stack, &mut root)?;
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml10_content());
                }
            }
            Event::CData(c) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&c.xml10_content());
                }
            }
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    let resolved = match r.resolve_char_ref()? {
                        Some(ch) => ch.to_string(),
                        None => match r.xml10_content().as_ref() {
                            "amp" => "&".into(),
                            "lt" => "<".into(),
                            "gt" => ">".into(),
                            "quot" => "\"".into(),
                            "apos" => "'".into(),
                            other => format!("&{other};"),
                        },
                    };
                    top.text.push_str(&resolved);
                }
            }
            Event::Eof => break,
            _ => {} // declaration, comments, PI, DOCTYPE — not data
        }
    }

    if !stack.is_empty() {
        bail!("XML truncated: unclosed element <{}>", stack.last().unwrap().name);
    }
    let root = root.context("XML contains no elements")?;

    let mut doc = Map::new();
    let name = root.name.clone();
    doc.insert(name, root.into_json());
    Ok(Value::Object(doc))
}

/// JSON → ComicInfo.xml.
///
/// Accepts both `{"ComicInfo": {...}}` and a "flat" object `{"Title": ...}` —
/// in the latter case it is wrapped in `<ComicInfo>`.
pub fn json_to_xml(value: &Value) -> Result<String> {
    let obj = value.as_object().context("ComicInfo metadata must be a JSON object")?;

    let (root_name, root_value) = match obj.iter().next() {
        Some((k, v)) if obj.len() == 1 && v.is_object() => (k.as_str(), v),
        _ => (ROOT, value),
    };

    let mut out = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    write_element(&mut out, root_name, root_value, 0)?;
    Ok(out)
}

fn scalar_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn write_element(out: &mut String, name: &str, value: &Value, depth: usize) -> Result<()> {
    if name.is_empty() || name.starts_with(['@', '#']) || name.contains(char::is_whitespace) {
        bail!("invalid XML element name: {name:?}");
    }
    let indent = "  ".repeat(depth);

    match value {
        Value::Array(items) => {
            for item in items {
                write_element(out, name, item, depth)?;
            }
        }
        Value::Null => out.push_str(&format!("{indent}<{name}/>\n")),
        Value::Object(obj) => {
            out.push_str(&format!("{indent}<{name}"));
            for (k, v) in obj.iter().filter(|(k, _)| k.starts_with('@')) {
                let v = scalar_to_string(v).with_context(|| format!("attribute {k} must be a scalar"))?;
                out.push_str(&format!(" {}=\"{}\"", &k[1..], escape(&v)));
            }

            let text = obj.get("#text").and_then(scalar_to_string);
            let children: Vec<_> = obj.iter().filter(|(k, _)| !k.starts_with(['@', '#'])).collect();

            match (text, children.is_empty()) {
                (None, true) => out.push_str("/>\n"),
                (Some(t), true) => out.push_str(&format!(">{}</{name}>\n", escape(&t))),
                (text, false) => {
                    out.push_str(">\n");
                    if let Some(t) = text {
                        out.push_str(&format!("{indent}  {}\n", escape(&t)));
                    }
                    for (k, v) in children {
                        write_element(out, k, v, depth + 1)?;
                    }
                    out.push_str(&format!("{indent}</{name}>\n"));
                }
            }
        }
        scalar => {
            let text = scalar_to_string(scalar).unwrap_or_default();
            out.push_str(&format!("{indent}<{name}>{}</{name}>\n", escape(&text)));
        }
    }
    Ok(())
}

/// Whether the JSON looks like converted ComicInfo (has a `ComicInfo` root).
pub fn is_comicinfo(value: &Value) -> bool {
    value.get(ROOT).is_some_and(Value::is_object)
}

fn root(value: &Value) -> Option<&Map<String, Value>> {
    value.get(ROOT)?.as_object()
}

fn field<'a>(value: &'a Value, name: &str) -> Option<&'a str> {
    root(value)?.get(name)?.as_str()
}

/// `<Manga>YesAndRightToLeft</Manga>` — right-to-left reading.
pub fn is_rtl(value: &Value) -> bool {
    field(value, "Manga") == Some("YesAndRightToLeft")
}

/// Chapter number from `<Number>`, if it is an integer.
pub fn chapter_number(value: &Value) -> Option<u16> {
    field(value, "Number")?.trim().parse().ok()
}

/// A non-empty ComicInfo text field.
pub fn text_field(value: &Value, name: &str) -> Option<String> {
    field(value, name).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Volume number from `<Volume>`.
pub fn volume_number(value: &Value) -> Option<u16> {
    field(value, "Volume")?.trim().parse().ok()
}

/// Removes fields describing a single chapter (`Title`, `Number`, `Pages`, `PageCount`, and with
/// several volumes — `Volume`): for a multi-chapter file they would be false.
/// Chapter titles and numbers are kept in the table of contents (`crate::contents`).
pub fn strip_chapter_fields(value: &mut Value, keep_volume: bool) {
    if let Some(obj) = value.get_mut(ROOT).and_then(Value::as_object_mut) {
        for key in ["Title", "Number", "Pages", "PageCount"] {
            obj.remove(key);
        }
        if !keep_volume {
            obj.remove("Volume");
        }
    }
}

/// Page roles from `<Pages><Page Image="N" Type="…" DoublePage="True"/></Pages>`.
/// Returns `(image index, role)` only for pages with a non-default role.
/// `<Pages>` for ComicInfo from page roles and sizes (the inverse of [`page_roles`]).
pub fn pages_json(pages: &[(PageRole, u32, u32)]) -> Value {
    let list: Vec<Value> = pages
        .iter()
        .enumerate()
        .map(|(i, &(role, width, height))| {
            let mut page = Map::new();
            page.insert("@Image".into(), Value::String(i.to_string()));
            let ty = match role {
                PageRole::FrontCover => Some("FrontCover"),
                PageRole::BackCover => Some("BackCover"),
                PageRole::TitlePage => Some("InnerCover"),
                PageRole::Advertisement => Some("Advertisement"),
                PageRole::Credits => Some("Editorial"),
                _ => None,
            };
            if let Some(ty) = ty {
                page.insert("@Type".into(), Value::String(ty.into()));
            }
            if role == PageRole::SpreadSingle {
                page.insert("@DoublePage".into(), Value::String("True".into()));
            }
            if width > 0 {
                page.insert("@ImageWidth".into(), Value::String(width.to_string()));
                page.insert("@ImageHeight".into(), Value::String(height.to_string()));
            }
            Value::Object(page)
        })
        .collect();
    serde_json::json!({ "Page": list })
}

pub fn page_roles(value: &Value) -> Vec<(usize, PageRole)> {
    let Some(pages) = root(value).and_then(|r| r.get("Pages")).and_then(|p| p.get("Page")) else {
        return Vec::new();
    };
    let list: Vec<&Value> = match pages {
        Value::Array(items) => items.iter().collect(),
        single => vec![single],
    };

    list.into_iter()
        .filter_map(|p| {
            let image = p.get("@Image")?.as_str()?.parse().ok()?;
            let double = p.get("@DoublePage").and_then(Value::as_str) == Some("True");
            let role = match p.get("@Type").and_then(Value::as_str).unwrap_or("Story") {
                "FrontCover" => PageRole::FrontCover,
                "BackCover" => PageRole::BackCover,
                "InnerCover" => PageRole::TitlePage,
                "Advertisement" => PageRole::Advertisement,
                "Editorial" | "Letters" => PageRole::Credits,
                _ if double => PageRole::SpreadSingle,
                _ => PageRole::Normal,
            };
            (role != PageRole::Normal).then_some((image, role))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version='1.0' encoding='UTF-8' ?>
<ComicInfo xmlns:xsd="http://www.w3.org/2001/XMLSchema">
  <Title>Volume 1 &amp; «Chapter» &#49;</Title>
  <Number>7</Number>
  <Summary><![CDATA[<b>bold</b>]]></Summary>
  <Manga>YesAndRightToLeft</Manga>
  <ty:Status xmlns:ty="http://example.com">Ongoing</ty:Status>
  <Pages>
    <Page Image="0" Type="FrontCover" ImageWidth="800"/>
    <Page Image="1"/>
    <Page Image="2" DoublePage="True"/>
  </Pages>
</ComicInfo>"#;

    #[test]
    fn converts_to_json() {
        let json = xml_to_json(SAMPLE).unwrap();
        let ci = &json["ComicInfo"];
        assert_eq!(ci["@xmlns:xsd"], "http://www.w3.org/2001/XMLSchema");
        assert_eq!(ci["Title"], "Volume 1 & «Chapter» 1");
        assert_eq!(ci["Summary"], "<b>bold</b>");
        assert_eq!(ci["ty:Status"]["#text"], "Ongoing");
        assert_eq!(ci["Pages"]["Page"].as_array().unwrap().len(), 3);
        assert_eq!(ci["Pages"]["Page"][0]["@Type"], "FrontCover");

        assert!(is_comicinfo(&json));
        assert!(is_rtl(&json));
        assert_eq!(chapter_number(&json), Some(7));
        assert_eq!(page_roles(&json), vec![(0, PageRole::FrontCover), (2, PageRole::SpreadSingle)]);
    }

    #[test]
    fn roundtrip_is_lossless_for_data() {
        let json = xml_to_json(SAMPLE).unwrap();
        let xml = json_to_xml(&json).unwrap();
        assert_eq!(xml_to_json(&xml).unwrap(), json);
    }

    #[test]
    fn flat_json_is_wrapped_into_comicinfo() {
        let xml = json_to_xml(&serde_json::json!({"Title": "A < B", "Number": 3})).unwrap();
        assert!(xml.contains("<ComicInfo>"));
        assert!(xml.contains("<Title>A &lt; B</Title>"));
        assert!(xml.contains("<Number>3</Number>"));
    }

    #[test]
    fn rejects_broken_xml() {
        assert!(xml_to_json("<ComicInfo><Title>x</ComicInfo>").is_err());
        assert!(xml_to_json("<A/><B/>").is_err());
    }
}
