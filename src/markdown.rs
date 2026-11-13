use std::path::Path;

use crate::chunk::Chunk;
use crate::symbol::Symbol;

/// A section shorter than this many non-blank lines is glued to the next one.
const TINY: usize = 3;

pub fn is_markdown(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        ["md", "markdown", "mdx"]
            .iter()
            .any(|m| e.eq_ignore_ascii_case(m))
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Blank,
    Front,
    FenceOpen,
    FenceBody,
    FenceClose,
    Heading,
    Underline,
    Para,
    Other,
}

struct Section {
    start: usize,
    end: usize,
    name: Option<String>,
    fixed: bool,
}

/// Cuts a document at its headings. None when it has none, so the caller falls
/// back to line windows.
pub fn chunk(text: &str, max_lines: usize) -> Option<Vec<Chunk>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let kinds = classify(&lines);
    let sections = sections(&lines, &kinds)?;
    let max = max_lines.max(1);
    let mut chunks = Vec::new();
    for section in merge(sections, &lines, max) {
        let spans = match section.end - section.start > max {
            true => split(&kinds, &lines, section.start, section.end, max),
            false => vec![(section.start, section.end)],
        };
        let symbol = section
            .name
            .filter(|name| !name.is_empty())
            .map(Symbol::heading);
        for (start, end) in spans {
            chunks.push(Chunk {
                start_line: start + 1,
                end_line: end,
                text: lines[start..end].join("\n"),
                symbol: symbol.clone(),
                declarations: Vec::new(),
            });
        }
    }
    Some(chunks)
}

fn classify(lines: &[&str]) -> Vec<Kind> {
    let mut kinds = Vec::with_capacity(lines.len());
    if let Some(close) = front_matter_end(lines) {
        kinds.resize(close + 1, Kind::Front);
    }
    let mut fence: Option<(char, usize)> = None;
    for line in &lines[kinds.len()..] {
        kinds.push(match fence {
            Some((mark, length)) => match closes_fence(line, mark, length) {
                true => {
                    fence = None;
                    Kind::FenceClose
                }
                false => Kind::FenceBody,
            },
            None => match opens_fence(line) {
                Some(open) => {
                    fence = Some(open);
                    Kind::FenceOpen
                }
                None => line_kind(line),
            },
        });
    }
    kinds
}

fn front_matter_end(lines: &[&str]) -> Option<usize> {
    if lines.first()?.trim_end() != "---" {
        return None;
    }
    lines[1..]
        .iter()
        .position(|l| matches!(l.trim_end(), "---" | "..."))
        .map(|i| i + 1)
}

/// The line without up to three spaces of indentation, which a block start
/// may carry; more is an indented code block.
fn unindented(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    (line.len() - rest.len() <= 3 && !rest.starts_with('\t')).then_some(rest)
}

fn opens_fence(line: &str) -> Option<(char, usize)> {
    let rest = unindented(line)?;
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let length = rest.chars().take_while(|&c| c == mark).count();
    let info = &rest[length..];
    (length >= 3 && !(mark == '`' && info.contains('`'))).then_some((mark, length))
}

fn closes_fence(line: &str, mark: char, length: usize) -> bool {
    let Some(rest) = unindented(line) else {
        return false;
    };
    let run = rest.chars().take_while(|&c| c == mark).count();
    run >= length && rest[run..].trim().is_empty()
}

fn line_kind(line: &str) -> Kind {
    if line.trim().is_empty() {
        return Kind::Blank;
    }
    let Some(rest) = unindented(line) else {
        return Kind::Other;
    };
    let hashes = rest.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes)
        && rest[hashes..]
            .chars()
            .next()
            .is_none_or(|c| matches!(c, ' ' | '\t'))
    {
        return Kind::Heading;
    }
    let bare = rest.trim_end();
    if bare.len() >= 2 && (bare.bytes().all(|b| b == b'=') || bare.bytes().all(|b| b == b'-')) {
        return Kind::Underline;
    }
    let marks = bare.chars().filter(|c| !c.is_whitespace()).count();
    let thematic = bare
        .chars()
        .all(|c| matches!(c, '*' | '_' | '-' | ' ' | '\t'));
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let numbered = digits > 0
        && rest[digits..].starts_with(['.', ')'])
        && rest[digits + 1..].starts_with([' ', '\t']);
    let marked =
        rest.starts_with(['>', '|', '<']) || ["- ", "* ", "+ "].iter().any(|m| rest.starts_with(m));
    match (thematic && marks >= 3) || numbered || marked {
        true => Kind::Other,
        false => Kind::Para,
    }
}

/// Where each section starts, ending at the next heading of any level. A
/// paragraph followed by a line of `=` or `-` is a heading too; the same line
/// after a blank one is a thematic break and starts nothing.
fn sections(lines: &[&str], kinds: &[Kind]) -> Option<Vec<Section>> {
    let mut heads: Vec<(usize, String)> = Vec::new();
    for (i, kind) in kinds.iter().enumerate() {
        match kind {
            Kind::Heading => heads.push((i, atx_name(lines[i]))),
            Kind::Underline if i > 0 && kinds[i - 1] == Kind::Para => {
                let mut start = i - 1;
                while start > 0 && kinds[start - 1] == Kind::Para {
                    start -= 1;
                }
                heads.push((start, plain(&lines[start..i].join(" "))));
            }
            _ => {}
        }
    }
    if heads.is_empty() {
        return None;
    }
    let front = kinds.iter().take_while(|&&k| k == Kind::Front).count();
    let mut bounds: Vec<(usize, Option<String>, bool)> = Vec::new();
    if front > 0 {
        bounds.push((0, None, true));
    }
    if heads[0].0 > front {
        bounds.push((front, None, false));
    }
    bounds.extend(heads.into_iter().map(|(at, name)| (at, Some(name), false)));
    let mut out = Vec::new();
    for (i, (start, name, fixed)) in bounds.iter().enumerate() {
        let end = bounds.get(i + 1).map_or(lines.len(), |next| next.0);
        let mut section = Section {
            start: *start,
            end,
            name: name.clone(),
            fixed: *fixed,
        };
        while section.start < section.end && lines[section.start].trim().is_empty() {
            section.start += 1;
        }
        while section.end > section.start && lines[section.end - 1].trim().is_empty() {
            section.end -= 1;
        }
        if section.start < section.end {
            out.push(section);
        }
    }
    Some(out)
}

fn atx_name(line: &str) -> String {
    let text = line.trim().trim_start_matches('#').trim();
    let closed = text.trim_end_matches('#');
    match closed.len() != text.len() && (closed.is_empty() || closed.ends_with([' ', '\t'])) {
        true => plain(closed),
        false => plain(text),
    }
}

/// The text of a heading without its markup: emphasis and code marks go, a
/// link or an image is reduced to its text, and an underscore inside a word
/// stays since it is part of the name.
fn plain(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let word = |at: Option<usize>| {
        at.and_then(|i| chars.get(i))
            .is_some_and(|c| c.is_alphanumeric())
    };
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' | '`' => {}
            '_' => {
                if word(i.checked_sub(1)) && word(Some(i + 1)) {
                    out.push('_');
                }
            }
            '!' if chars.get(i + 1) == Some(&'[') && link_end(&chars, i + 1).is_some() => {}
            '[' => match link_end(&chars, i) {
                Some((close, after)) => {
                    out.push_str(&plain(&chars[i + 1..close].iter().collect::<String>()));
                    i = after;
                    continue;
                }
                None => out.push('['),
            },
            c => out.push(c),
        }
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// For a `[` at `open` that starts `[text](target)` or `[text][label]`: where
/// the text ends and where the whole link does.
fn link_end(chars: &[char], open: usize) -> Option<(usize, usize)> {
    let close = open + chars[open..].iter().position(|&c| c == ']')?;
    let closer = match chars.get(close + 1)? {
        '(' => ')',
        '[' => ']',
        _ => return None,
    };
    let last = close + 1 + chars[close + 1..].iter().position(|&c| c == closer)?;
    Some((close, last + 1))
}

/// Sections shorter than [`TINY`] non-blank lines join the one after them, so a
/// heading stays with its first child, as long as the result fits a chunk.
fn merge(sections: Vec<Section>, lines: &[&str], max: usize) -> Vec<Section> {
    let tiny = |s: &Section| {
        lines[s.start..s.end]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .count()
            < TINY
    };
    let mut out = Vec::with_capacity(sections.len());
    let mut rest = sections.into_iter().peekable();
    while let Some(mut section) = rest.next() {
        while !section.fixed && tiny(&section) {
            match rest.next_if(|next| !next.fixed && next.end - section.start <= max) {
                Some(next) => {
                    section.end = next.end;
                    section.name = section.name.or(next.name);
                }
                None => break,
            }
        }
        out.push(section);
    }
    out
}

/// Pieces of a section longer than `max` lines, cut after a blank line or
/// beside a fenced block. A block longer than the limit is cut by lines.
fn split(
    kinds: &[Kind],
    lines: &[&str],
    start: usize,
    end: usize,
    max: usize,
) -> Vec<(usize, usize)> {
    let breaks = |k: usize| {
        !matches!(kinds[k], Kind::FenceBody | Kind::FenceClose)
            && (kinds[k - 1] == Kind::Blank
                || kinds[k] == Kind::FenceOpen
                || kinds[k - 1] == Kind::FenceClose)
    };
    let holds_body = |from: usize, to: usize| {
        lines[from..to]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .count()
            >= TINY
    };
    let mut out = Vec::new();
    let mut from = start;
    while from < end {
        while from < end && lines[from].trim().is_empty() {
            from += 1;
        }
        if from >= end {
            break;
        }
        let limit = from + max;
        let next = match limit >= end {
            true => end,
            false => (from + 1..=limit)
                .rev()
                .find(|&k| breaks(k) && holds_body(from, k))
                .unwrap_or(limit),
        };
        let mut to = next;
        while lines[to - 1].trim().is_empty() {
            to -= 1;
        }
        out.push((from, to));
        from = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str) -> Vec<Chunk> {
        chunk(text, 50).expect("headings")
    }

    fn spans(chunks: &[Chunk]) -> Vec<(usize, usize)> {
        chunks.iter().map(|c| (c.start_line, c.end_line)).collect()
    }

    fn names(chunks: &[Chunk]) -> Vec<String> {
        chunks
            .iter()
            .map(|c| c.symbol.as_ref().map(Symbol::label).unwrap_or_default())
            .collect()
    }

    fn body(n: usize) -> String {
        (1..=n).map(|i| format!("line {i}\n")).collect()
    }

    #[test]
    fn the_extension_picks_the_chunker() {
        for file in ["a.md", "A.MD", "b.markdown", "c.mdx", "d/e.Md"] {
            assert!(is_markdown(Path::new(file)), "{file}");
        }
        for file in ["a.txt", "md", "a.md.bak", "a.rs"] {
            assert!(!is_markdown(Path::new(file)), "{file}");
        }
    }

    #[test]
    fn every_level_of_heading_opens_a_section() {
        let text = (1..=6)
            .map(|n| format!("{} H{n}\na\nb\nc\n", "#".repeat(n)))
            .collect::<String>();
        let chunks = cut(&text);
        assert_eq!(
            spans(&chunks),
            [(1, 4), (5, 8), (9, 12), (13, 16), (17, 20), (21, 24)]
        );
        assert_eq!(names(&chunks)[5], "heading H6");
    }

    #[test]
    fn a_hashtag_and_a_deep_or_indented_hash_are_not_headings() {
        let text = "# Real\na\nb\n#hashtag\n####### seven\n    # code\nc\n";
        assert_eq!(spans(&cut(text)), [(1, 7)]);
        let text = "# Real\na\nb\n   ## indented by three\nc\nd\ne\n";
        assert_eq!(spans(&cut(text)), [(1, 3), (4, 7)]);
    }

    #[test]
    fn an_empty_heading_and_a_closing_sequence_are_read() {
        let chunks = cut("#\na\nb\nc\n## Name ##\nd\ne\nf\n");
        assert_eq!(names(&chunks), ["", "heading Name"]);
    }

    #[test]
    fn setext_headings_need_a_paragraph_above() {
        let text =
            "Title\n=====\na\nb\n\nSub\nfirst line\n---\nc\nd\n\n---\n\nrule above\n===\ne\n";
        let chunks = cut(text);
        assert_eq!(spans(&chunks), [(1, 4), (6, 12), (14, 16)]);
        assert_eq!(
            names(&chunks)[..2],
            ["heading Title", "heading Sub first line"]
        );
        assert!(chunks[1].text.starts_with("Sub\nfirst line\n---"));
    }

    #[test]
    fn a_thematic_break_after_a_blank_line_cuts_nothing() {
        let chunks = cut("# A\nx\ny\n\n---\n\nz\nw\n");
        assert_eq!(spans(&chunks), [(1, 8)]);
    }

    #[test]
    fn front_matter_is_a_section_of_its_own() {
        let text = "---\ntitle: x\n# not a heading\n---\n# Doc\na\nb\nc\n";
        let chunks = cut(text);
        assert_eq!(spans(&chunks), [(1, 4), (5, 8)]);
        assert_eq!(names(&chunks), ["", "heading Doc"]);
        let chunks = cut("---\n# Doc\na\nb\nc\n");
        assert_eq!(spans(&chunks), [(1, 5)]);
    }

    #[test]
    fn hashes_inside_fences_are_text() {
        let text = "# A\n```sh\n# comment\n```\n~~~\n## also\n~~~\nx\n\n\
                    ````md\n```\n# nested\n```\n````\n~~~\n# in tildes\n~~~\n";
        assert_eq!(spans(&cut(text)), [(1, 17)]);
        let text = "# A\n```\n# no\n~~~\n# still no\n```\n# B\nb\nc\nd\n";
        assert_eq!(spans(&cut(text)), [(1, 6), (7, 10)]);
    }

    #[test]
    fn an_unclosed_fence_runs_to_the_end() {
        let text = "# A\na\nb\n```\n# not a heading\nmore\n";
        assert_eq!(spans(&cut(text)), [(1, 6)]);
    }

    #[test]
    fn crlf_line_endings_give_the_same_cut() {
        let lf = "# A\na\nb\nc\n\n## B\nd\ne\nf\n";
        let crlf = lf.replace('\n', "\r\n");
        let (a, b) = (cut(lf), cut(&crlf));
        assert_eq!(spans(&a), spans(&b));
        assert!(b.iter().all(|c| !c.text.contains('\r')));
    }

    #[test]
    fn a_leading_byte_order_mark_does_not_hide_the_first_heading() {
        let chunks = cut("\u{feff}# Title\na\nb\nc\n");
        assert_eq!(names(&chunks), ["heading Title"]);
        assert_eq!(chunks[0].text, "# Title\na\nb\nc");
    }

    #[test]
    fn a_file_without_headings_is_left_to_the_line_windows() {
        assert!(chunk("just text\n\nmore\n", 50).is_none());
        assert!(chunk("```\n# in a fence\n```\n", 50).is_none());
        assert!(chunk("", 50).is_none());
    }

    #[test]
    fn text_before_the_first_heading_is_a_chunk_of_its_own() {
        let chunks = cut("intro one\nintro two\nintro three\n\n# A\na\nb\nc\n");
        assert_eq!(spans(&chunks), [(1, 3), (5, 8)]);
        assert_eq!(names(&chunks), ["", "heading A"]);
    }

    #[test]
    fn heading_names_lose_their_markup() {
        let name = |h: &str| {
            let chunks = chunk(&format!("{h}\na\nb\nc\n"), 50).unwrap();
            chunks[0].symbol.clone().unwrap().name
        };
        assert_eq!(
            name("## The `chunk_file` *function* ##"),
            "The chunk_file function"
        );
        assert_eq!(
            name("# _Emphasis_ and snake_case"),
            "Emphasis and snake_case"
        );
        assert_eq!(
            name("# See [the guide](docs/guide.md) and ![logo](a.png) [ref][1]"),
            "See the guide and logo ref"
        );
        assert_eq!(name("# [WIP] thing"), "[WIP] thing");
        assert_eq!(name("# C# basics #"), "C# basics");
    }

    #[test]
    fn a_long_section_is_cut_at_blank_lines() {
        let paragraph = |n: usize| (0..n).map(|i| format!("p{i}\n")).collect::<String>();
        let text = format!(
            "# Long\n{}\n{}\n{}",
            paragraph(20),
            paragraph(20),
            paragraph(20)
        );
        let chunks = chunk(&text, 25).unwrap();
        assert_eq!(spans(&chunks), [(1, 21), (23, 42), (44, 63)]);
        assert!(names(&chunks).iter().all(|n| n == "heading Long"));
    }

    #[test]
    fn a_fenced_block_is_not_cut_when_it_fits() {
        let code: String = (0..10).map(|i| format!("code {i}\n")).collect();
        let text = format!("# A\n{}\n```\n{code}```\n\n{}", body(8), body(8));
        let chunks = chunk(&text, 16).unwrap();
        assert!(chunks.len() > 1);
        for c in &chunks {
            assert_eq!(c.text.matches("```").count() % 2, 0, "{}", c.text);
            assert!(c.end_line - c.start_line < 16);
        }
    }

    #[test]
    fn a_fenced_block_longer_than_the_limit_is_cut_by_lines() {
        let code: String = (1..=30).map(|i| format!("c{i}\n")).collect();
        let text = format!("# A\nintro\nintro\nintro\n```\n{code}```\nafter\nafter\nafter\n");
        let chunks = chunk(&text, 12).unwrap();
        assert_eq!(spans(&chunks), [(1, 4), (5, 16), (17, 28), (29, 39)]);
    }

    #[test]
    fn a_tiny_section_stays_with_its_first_child() {
        let text =
            "# Guide\n\n## Install\nrun it\nthen this\nand that\n\n## Use\none\ntwo\nthree\n";
        let chunks = cut(text);
        assert_eq!(spans(&chunks), [(1, 6), (8, 11)]);
        assert_eq!(names(&chunks), ["heading Guide", "heading Use"]);
    }

    #[test]
    fn a_tiny_section_is_not_glued_past_the_limit() {
        let text = format!("# Guide\n\n## Install\n{}", body(8));
        let chunks = chunk(&text, 10).unwrap();
        assert_eq!(spans(&chunks), [(1, 1), (3, 11)]);
    }

    #[test]
    fn front_matter_is_never_merged() {
        let chunks = cut("---\nt: 1\n---\n# A\na\nb\nc\n");
        assert_eq!(spans(&chunks), [(1, 3), (4, 7)]);
    }

    #[test]
    fn every_non_blank_line_is_in_exactly_one_chunk() {
        let text = "\u{feff}\n\nintro\n\n---\nowner: me\n---\n\
                    # One\n\n## Two\n\nSetext\n======\nbody\n\n```\n# fenced\n\n## fenced\n```\n\n\
                    - item\n- item\n\n***\n\nlast\n\n#hash\n\nTitle\n---\nx\n\n~~~~\n~~~\n# in\n~~~~\n\n# End\n";
        let long = format!("{text}\n## Long\n{}", body(130));
        for (text, max) in [
            (text, 50),
            (text, 3),
            (text, 1),
            (long.as_str(), 50),
            (long.as_str(), 7),
        ] {
            let plain = text.strip_prefix('\u{feff}').unwrap_or(text);
            let lines: Vec<&str> = plain.lines().collect();
            let chunks = chunk(text, max).expect("headings");
            let mut seen = vec![0usize; lines.len()];
            let mut last_end = 0;
            for c in &chunks {
                assert!(
                    c.start_line > last_end,
                    "overlap at {}: {max}",
                    c.start_line
                );
                assert!(c.start_line <= c.end_line && c.end_line <= lines.len());
                assert_eq!(c.text, lines[c.start_line - 1..c.end_line].join("\n"));
                assert!(!crate::chunk::is_blank(&c.text), "{max}: {c:?}");
                last_end = c.end_line;
                for n in c.start_line..=c.end_line {
                    seen[n - 1] += 1;
                }
            }
            for (i, line) in lines.iter().enumerate() {
                match line.trim().is_empty() {
                    true => assert!(seen[i] <= 1),
                    false => assert_eq!(seen[i], 1, "line {} {line:?} with max {max}", i + 1),
                }
            }
        }
    }
}
