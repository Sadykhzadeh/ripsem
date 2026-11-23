//! Reads and edits JSON with comments and trailing commas, the form VS Code and
//! OpenCode accept in their config files. The reader only finds byte offsets in
//! the text it is given; every edit splices that text, so the user's comments,
//! layout and key order stay as they were. Anything the reader is not sure of is
//! an error, and the caller refuses the file rather than guess where to write.

use serde_json::Value;

/// An object as found in the text: where its braces are, and its members.
#[derive(Debug)]
pub struct Object {
    pub open: usize,
    pub close: usize,
    pub members: Vec<Member>,
}

/// One `"key": value` pair. `comma` is the comma after the value, if any; on the
/// last member that is a trailing comma.
#[derive(Debug)]
pub struct Member {
    pub key_start: usize,
    pub key: String,
    pub value: Node,
    pub comma: Option<usize>,
}

#[derive(Debug)]
pub enum Node {
    Object(Object),
    /// Any other value, by its byte span: a string, number, literal or array.
    Other {
        start: usize,
        end: usize,
    },
}

impl Node {
    /// The byte span the value occupies, from its first byte to just past its last.
    pub fn span(&self) -> (usize, usize) {
        match self {
            Node::Object(object) => (object.open, object.close + 1),
            Node::Other { start, end } => (*start, *end),
        }
    }
}

impl Object {
    /// The member named `key`. Two members with one name are an error: which of
    /// them a reader would use is not for this module to decide.
    pub fn find(&self, key: &str) -> Result<Option<&Member>, String> {
        let mut found = None;
        for member in self.members.iter().filter(|m| m.key == key) {
            if found.is_some() {
                return Err(format!("'{key}' is there more than once"));
            }
            found = Some(member);
        }
        Ok(found)
    }
}

/// The root object of `text`, or `None` when the text is not one JSON object
/// with comments and trailing commas allowed. The text must not start with a
/// byte-order mark.
pub fn parse(text: &str) -> Option<Object> {
    let mut parser = Parser { text, pos: 0 };
    parser.skip()?;
    let root = parser.object()?;
    parser.skip()?;
    (parser.pos == text.len()).then_some(root)
}

struct Parser<'a> {
    text: &'a str,
    pos: usize,
}

impl Parser<'_> {
    fn byte(&self) -> Option<u8> {
        self.text.as_bytes().get(self.pos).copied()
    }

    /// Skips white space, `//` comments and `/* */` comments. An unterminated
    /// block comment is an error.
    fn skip(&mut self) -> Option<()> {
        loop {
            match &self.text.as_bytes()[self.pos..] {
                [b' ' | b'\t' | b'\r' | b'\n', ..] => self.pos += 1,
                [b'/', b'/', ..] => {
                    self.pos = match self.text[self.pos..].find('\n') {
                        Some(i) => self.pos + i + 1,
                        None => self.text.len(),
                    };
                }
                [b'/', b'*', ..] => {
                    let end = self.text[self.pos + 2..].find("*/")?;
                    self.pos += end + 4;
                }
                _ => return Some(()),
            }
        }
    }

    fn value(&mut self) -> Option<Node> {
        self.skip()?;
        let start = self.pos;
        match self.byte()? {
            b'{' => self.object().map(Node::Object),
            b'[' => {
                self.array()?;
                Some(Node::Other {
                    start,
                    end: self.pos,
                })
            }
            b'"' => {
                self.string()?;
                Some(Node::Other {
                    start,
                    end: self.pos,
                })
            }
            b'-' | b'0'..=b'9' | b'a'..=b'z' => {
                while matches!(self.byte(), Some(b) if b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
                {
                    self.pos += 1;
                }
                // Numbers and the three literals are checked by serde_json, so a
                // mistyped one is not taken for a value.
                serde_json::from_str::<Value>(&self.text[start..self.pos]).ok()?;
                Some(Node::Other {
                    start,
                    end: self.pos,
                })
            }
            _ => None,
        }
    }

    /// Steps over a string literal, the opening quote at `pos`.
    fn string(&mut self) -> Option<()> {
        self.pos += 1;
        loop {
            match self.byte()? {
                b'\\' => self.pos += 2,
                b'"' => {
                    self.pos += 1;
                    return Some(());
                }
                b'\n' | b'\r' => return None,
                _ => self.pos += 1,
            }
        }
    }

    fn array(&mut self) -> Option<()> {
        self.pos += 1;
        loop {
            self.skip()?;
            if self.byte()? == b']' {
                self.pos += 1;
                return Some(());
            }
            self.value()?;
            self.skip()?;
            match self.byte()? {
                b',' => self.pos += 1,
                b']' => {}
                _ => return None,
            }
        }
    }

    fn object(&mut self) -> Option<Object> {
        let open = self.pos;
        self.pos += 1;
        let mut members = Vec::new();
        loop {
            self.skip()?;
            if self.byte()? == b'}' {
                let close = self.pos;
                self.pos += 1;
                return Some(Object {
                    open,
                    close,
                    members,
                });
            }
            let key_start = self.pos;
            if self.byte()? != b'"' {
                return None;
            }
            self.string()?;
            let key: String = serde_json::from_str(&self.text[key_start..self.pos]).ok()?;
            self.skip()?;
            if self.byte()? != b':' {
                return None;
            }
            self.pos += 1;
            let value = self.value()?;
            self.skip()?;
            let comma = match self.byte()? {
                b',' => {
                    self.pos += 1;
                    Some(self.pos - 1)
                }
                b'}' => None,
                _ => return None,
            };
            members.push(Member {
                key_start,
                key,
                value,
                comma,
            });
        }
    }
}

/// How a member written into an object is laid out: the file's line break, the
/// indentation of its members, the indentation of the closing brace, and one
/// indentation step. `multi` is false for an object written on one line.
pub struct Style {
    pub newline: String,
    pub indent: String,
    pub close_indent: String,
    pub unit: String,
    pub multi: bool,
}

/// `"key": value` as it is written into the file under `style`.
pub fn member_text(key: &str, value: &Value, style: &Style) -> String {
    let rendered = if style.multi {
        let mut pretty = Vec::new();
        let format = serde_json::ser::PrettyFormatter::with_indent(style.unit.as_bytes());
        let mut serializer = serde_json::Serializer::with_formatter(&mut pretty, format);
        serde::Serialize::serialize(value, &mut serializer).expect("a JSON value serializes");
        String::from_utf8_lossy(&pretty)
            .replace('\n', &format!("{}{}", style.newline, style.indent))
    } else {
        value.to_string()
    };
    format!("{}: {rendered}", Value::from(key))
}

/// `text` with `member` added to `object` as its last member, in `style`. The
/// member goes after the last value, or after its trailing comma when there is
/// one, so the object's own comments and closing brace stay where they were.
pub fn insert(text: &str, object: &Object, member: &str, style: &Style) -> String {
    let (at, piece) = match object.members.last() {
        None => {
            let inner = &text[object.open + 1..object.close];
            if inner.trim().is_empty() {
                let (before, after) = match style.multi {
                    true => (
                        format!("{}{}", style.newline, style.indent),
                        format!("{}{}", style.newline, style.close_indent),
                    ),
                    false => (String::new(), String::new()),
                };
                return format!(
                    "{}{before}{member}{after}{}",
                    &text[..object.open + 1],
                    &text[object.close..]
                );
            }
            let piece = match style.multi {
                true => format!("{}{}{member}", style.newline, style.indent),
                false => member.to_string(),
            };
            (object.open + 1, piece)
        }
        Some(last) => match (last.comma, style.multi) {
            (Some(comma), true) => (
                comma + 1,
                format!("{}{}{member},", style.newline, style.indent),
            ),
            (Some(comma), false) => (comma + 1, format!(" {member},")),
            (None, true) => (
                last.value.span().1,
                format!(",{}{}{member}", style.newline, style.indent),
            ),
            (None, false) => (last.value.span().1, format!(", {member}")),
        },
    };
    format!("{}{piece}{}", &text[..at], &text[at..])
}

/// `text` with the member named `key` removed, or `None` when there is no such
/// member. When the member is the last one that `insert` added, the bytes
/// removed are exactly the ones it added, so the original text comes back. An
/// object emptied by the removal is left as `{}` when it held nothing else.
pub fn remove(text: &str, object: &Object, key: &str) -> Result<Option<String>, String> {
    let mut hits = object
        .members
        .iter()
        .enumerate()
        .filter(|(_, m)| m.key == key);
    let Some((index, member)) = hits.next() else {
        return Ok(None);
    };
    if hits.next().is_some() {
        return Err(format!("'{key}' is there more than once"));
    }
    let members = &object.members;
    let (start, end) = match (index, members.len()) {
        (_, 1) => {
            let end = member.comma.map_or(member.value.span().1, |c| c + 1);
            let around = &text[object.open + 1..member.key_start];
            let after = &text[end..object.close];
            if around.trim().is_empty() && after.trim().is_empty() {
                return Ok(Some(format!(
                    "{}{}",
                    &text[..object.open + 1],
                    &text[object.close..]
                )));
            }
            (member.key_start, end)
        }
        (0, _) => (member.key_start, members[1].key_start),
        // Every member but the last has a comma. Removing from that comma takes
        // the separator `insert` put in front of the member, and leaves the
        // member's own trailing comma, if it has one, as the object had it.
        (i, _) => (
            members[i - 1]
                .comma
                .unwrap_or(members[i - 1].value.span().1),
            member.value.span().1,
        ),
    };
    Ok(Some(format!("{}{}", &text[..start], &text[end..])))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn style(multi: bool) -> Style {
        Style {
            newline: "\n".into(),
            indent: "    ".into(),
            close_indent: "  ".into(),
            unit: "  ".into(),
            multi,
        }
    }

    fn entry() -> Value {
        json!({ "command": "ripsem", "args": ["mcp"] })
    }

    fn add(text: &str, key: &str, style: &Style) -> String {
        let root = parse(text).expect("the test text parses");
        insert(text, &root, &member_text(key, &entry(), style), style)
    }

    #[test]
    fn reads_comments_and_trailing_commas() {
        let text = "// header\n{\n  /* a */ \"x\": { \"y\": [1, 2,], }, // tail\n}\n";
        let root = parse(text).expect("parses");
        assert_eq!(root.members.len(), 1);
        assert_eq!(root.members[0].key, "x");
        assert!(root.members[0].comma.is_some());
        match &root.members[0].value {
            Node::Object(inner) => assert_eq!(inner.members[0].key, "y"),
            other => panic!("not an object: {other:?}"),
        }
    }

    #[test]
    fn refuses_what_it_cannot_be_sure_of() {
        for bad in [
            "{",
            "{\"a\":1",
            "{\"a\" 1}",
            "{\"a\":1 \"b\":2}",
            "{\"a\":tru}",
            "{\"a\":1} x",
            "{\"a\":1} /* open",
            "[1]",
            "{\"a\":\"x\ny\"}",
            "{,}",
            "{\"a\":1,,}",
        ] {
            assert!(parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_key_inside_a_comment_or_a_string_is_not_a_member() {
        let text = "{ // \"servers\": {}\n  \"x\": \"\\\"servers\\\": {}\" }";
        let root = parse(text).expect("parses");
        let keys: Vec<&str> = root.members.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["x"]);
        assert!(root.find("servers").unwrap().is_none());
    }

    #[test]
    fn a_key_given_twice_is_an_error_not_a_guess() {
        let root = parse("{\"a\": 1, \"a\": 2}").expect("parses");
        assert!(root.find("a").is_err());
    }

    #[test]
    fn a_brace_inside_a_string_does_not_close_the_object() {
        let text = "{\"s\": \"} // }\", \"t\": {}}";
        let root = parse(text).expect("parses");
        assert_eq!(root.members.len(), 2);
        assert_eq!(root.close, text.len() - 1);
    }

    #[test]
    fn a_member_goes_after_the_last_value_with_its_comma() {
        let text = "{\n    \"a\": {}\n}\n";
        assert_eq!(
            add(text, "b", &style(true)),
            "{\n    \"a\": {},\n    \"b\": {\n      \"args\": [\n        \"mcp\"\n      ],\n      \"command\": \"ripsem\"\n    }\n}\n"
        );
    }

    #[test]
    fn a_member_goes_after_a_trailing_comma_and_keeps_it() {
        let text = "{\n    \"a\": {},\n}\n";
        let out = add(text, "b", &style(true));
        assert!(out.starts_with("{\n    \"a\": {},\n    \"b\": {"), "{out}");
        assert!(out.ends_with("    },\n}\n"), "{out}");
        assert!(parse(&out).is_some(), "{out}");
    }

    #[test]
    fn an_empty_object_takes_the_member_on_its_own_lines() {
        let out = add("{\n}\n", "b", &style(true));
        assert_eq!(out.matches("\"b\"").count(), 1);
        assert!(parse(&out).is_some(), "{out}");
        assert_eq!(
            add("{}", "b", &style(false)),
            "{\"b\": {\"args\":[\"mcp\"],\"command\":\"ripsem\"}}"
        );
    }

    #[test]
    fn a_member_goes_inline_into_an_object_on_one_line() {
        let text = "{\"a\": 1, \"s\": {\"x\": 2}}";
        let root = parse(text).expect("parses");
        let inner = match &root.find("s").unwrap().unwrap().value {
            Node::Object(object) => object,
            other => panic!("{other:?}"),
        };
        let member = member_text("b", &entry(), &style(false));
        assert_eq!(
            insert(text, inner, &member, &style(false)),
            "{\"a\": 1, \"s\": {\"x\": 2, \"b\": {\"args\":[\"mcp\"],\"command\":\"ripsem\"}}}"
        );
    }

    #[test]
    fn removing_the_last_member_gives_back_the_text_it_was_added_to() {
        let original = "{\n    // keep me\n    \"a\": {\n        \"command\": \"x\"\n    },\n    \"c\": 3\n}\n";
        let added = add(original, "b", &style(true));
        let back = remove(&added, &parse(&added).unwrap(), "b")
            .unwrap()
            .unwrap();
        assert_eq!(back, original);

        let trailing = "{\n    \"a\": {},\n}\n";
        let added = add(trailing, "b", &style(true));
        let back = remove(&added, &parse(&added).unwrap(), "b")
            .unwrap()
            .unwrap();
        assert_eq!(back, trailing);
    }

    #[test]
    fn removing_the_only_member_empties_the_object_back_to_braces() {
        let added = add("{}", "b", &style(true));
        let back = remove(&added, &parse(&added).unwrap(), "b")
            .unwrap()
            .unwrap();
        assert_eq!(back, "{}");
    }

    #[test]
    fn removing_a_missing_key_changes_nothing() {
        let text = "{\"a\": 1}";
        assert_eq!(remove(text, &parse(text).unwrap(), "b").unwrap(), None);
    }
}
