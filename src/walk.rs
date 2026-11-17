use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ignore::overrides::OverrideBuilder;
use ignore::{WalkBuilder, WalkState};
use rayon::prelude::*;

use crate::config::Config;
use crate::fingerprint::{FileStat, content_hash};

/// Machine-written files that are committed on purpose, so `.gitignore` does not
/// cover them. A lock file can outweigh all the hand-written source in a repo.
const GENERATED: &[&str] = &[
    "*.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "packages.lock.json",
    "go.sum",
    "*.min.js",
    "*.min.css",
    "*.map",
];

pub fn collect(config: &Config) -> Result<Vec<PathBuf>> {
    // The text/binary check opens every file and is most of the cost, so it
    // runs in parallel.
    Ok(list(config)?
        .into_par_iter()
        .map(|(path, _)| path)
        .filter_map(|path| {
            matches!(read_hashed(&path, &mut Vec::new()), Sniffed::Text(_)).then_some(path)
        })
        .collect())
}

/// What the walk left out of the tree, by the rule that took each file out. The
/// counts add up: every file the walk reaches is indexed or left out for one
/// reason, and a file the exclude patterns drop is counted as excluded.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Tally {
    pub indexed: usize,
    pub binary: usize,
    pub minified: usize,
    pub blank: usize,
    pub unreadable: usize,
    pub too_large: usize,
    pub excluded: usize,
}

impl Tally {
    /// Every file the walk reached, indexed or not.
    pub fn seen(&self) -> usize {
        self.indexed
            + self.binary
            + self.minified
            + self.blank
            + self.unreadable
            + self.too_large
            + self.excluded
    }
}

enum Kind {
    Indexed,
    Binary,
    Minified,
    Blank,
    Unreadable,
}

fn kind_of(path: &Path) -> Kind {
    let Ok(bytes) = std::fs::read(path) else {
        return Kind::Unreadable;
    };
    let sniffed = &bytes[..bytes.len().min(SNIFF_BYTES as usize)];
    if !reads_as_text(sniffed) {
        Kind::Binary
    } else if is_minified(&bytes) {
        Kind::Minified
    } else if crate::chunk::is_blank(&decode_text(&bytes)) {
        Kind::Blank
    } else {
        Kind::Indexed
    }
}

/// The files [`collect`] returns, with a [`Tally`] of every other file the walk
/// reaches. It walks the tree three times and reads each file within the size
/// limit, so it is for `index --dry-run`, not for a normal run.
pub fn tally(config: &Config) -> Result<(Vec<PathBuf>, Tally)> {
    let max_bytes = config.max_file_kb.saturating_mul(1024);
    let everything = walk_files(config, false, u64::MAX)?.len();
    let reached = walk_files(config, true, u64::MAX)?.len();
    let kept = walk_files(config, true, max_bytes)?;
    let mut counts = Tally {
        excluded: everything.saturating_sub(reached),
        too_large: reached.saturating_sub(kept.len()),
        ..Tally::default()
    };
    let kinds: Vec<(PathBuf, Kind)> = kept
        .into_par_iter()
        .map(|(path, _)| {
            let kind = kind_of(&path);
            (path, kind)
        })
        .collect();
    let mut indexed = Vec::new();
    for (path, kind) in kinds {
        match kind {
            Kind::Indexed => {
                counts.indexed += 1;
                indexed.push(path);
            }
            Kind::Binary => counts.binary += 1,
            Kind::Minified => counts.minified += 1,
            Kind::Blank => counts.blank += 1,
            Kind::Unreadable => counts.unreadable += 1,
        }
    }
    Ok((indexed, counts))
}

/// What reading a file for the index found.
pub enum Sniffed {
    Text(u64),
    Binary,
    Unreadable,
}

/// Reads a file once for both the text check and the hash, where checking
/// first and hashing after would open it twice. `buffer` is reused between
/// calls.
pub fn read_hashed(path: &Path, buffer: &mut Vec<u8>) -> Sniffed {
    buffer.clear();
    if std::fs::File::open(path)
        .and_then(|mut file| file.read_to_end(buffer))
        .is_err()
    {
        return Sniffed::Unreadable;
    }
    let sniffed = &buffer[..buffer.len().min(SNIFF_BYTES as usize)];
    match reads_as_text(sniffed) && !is_minified(buffer) {
        true => Sniffed::Text(content_hash(buffer)),
        false => Sniffed::Binary,
    }
}

/// A file of at least this many bytes whose lines average at least
/// [`MINIFIED_LINE`] bytes is a minified or generated bundle. Its words are
/// noise to a search, and its chunks cut one long line again and again, so it
/// is left out like a binary file. A long line among ordinary ones does not
/// make a file minified.
const MINIFIED_BYTES: usize = 8 * 1024;
const MINIFIED_LINE: usize = 4 * 1024;

fn is_minified(bytes: &[u8]) -> bool {
    let lines = bytes.iter().filter(|&&b| b == b'\n').count() + 1;
    bytes.len() >= MINIFIED_BYTES && bytes.len() / lines >= MINIFIED_LINE
}

/// Every file the walk reaches within the size limit, in path order, with its
/// [`FileStat`]. Nothing is read: the size and time come from the directory
/// listing, which on Windows costs no system call.
pub fn list(config: &Config) -> Result<Vec<(PathBuf, FileStat)>> {
    walk_files(config, true, config.max_file_kb.saturating_mul(1024))
}

/// [`list`] with the exclude patterns and the size limit applied only when asked
/// for, so [`tally`] can see what each rule takes out.
fn walk_files(config: &Config, patterns: bool, max_bytes: u64) -> Result<Vec<(PathBuf, FileStat)>> {
    let mut builder = builder(config, patterns)?;
    // Pruned during the walk so it never descends into `.git`.
    let root = config.root.clone();
    builder.filter_entry(move |entry| !is_internal(entry.path(), &root));
    let (sender, receiver) = std::sync::mpsc::channel();
    builder.build_parallel().run(|| {
        let sender = sender.clone();
        Box::new(move |result| {
            if let Ok(entry) = result
                && entry.file_type().is_some_and(|t| t.is_file())
                && let Ok(meta) = entry.metadata()
                && meta.len() > 0
                && meta.len() <= max_bytes
            {
                let _ = sender.send((entry.into_path(), FileStat::of(&meta)));
            }
            WalkState::Continue
        })
    });
    drop(sender);
    // Threads finish in any order; the index lists files in one.
    let mut candidates: Vec<(PathBuf, FileStat)> = receiver.into_iter().collect();
    sort_by_path(&mut candidates);
    Ok(candidates)
}

/// Orders files the way `Path`'s own ordering does, component by component, so
/// the index keeps listing them in the same order. On Unix the bytes of a path
/// give that order once its separator is read as the lowest byte: `a-b` comes
/// before `a/b` as bytes, but `a` is the shorter component, so `a/b` comes
/// first. Each key is built once per file, not once per comparison, which costs
/// far less than walking the components on every comparison.
#[cfg(unix)]
fn sort_by_path(files: &mut [(PathBuf, FileStat)]) {
    use std::os::unix::ffi::OsStrExt;
    files.sort_by_cached_key(|(path, _)| {
        path.as_os_str()
            .as_bytes()
            .iter()
            .map(|&byte| if byte == b'/' { 0 } else { byte })
            .collect::<Vec<u8>>()
    });
}

#[cfg(not(unix))]
fn sort_by_path(files: &mut [(PathBuf, FileStat)]) {
    files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
}

fn builder(config: &Config, patterns: bool) -> Result<WalkBuilder> {
    let mut builder = WalkBuilder::new(&config.root);
    builder
        .hidden(false)
        .require_git(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .add_custom_ignore_filename(".ripsemignore");

    let defaults = match config.skip_generated && patterns {
        true => GENERATED,
        false => &[],
    };
    let excludes: &[String] = match patterns {
        true => &config.exclude,
        false => &[],
    };
    if !excludes.is_empty() || !defaults.is_empty() {
        let mut overrides = OverrideBuilder::new(&config.root);
        for pattern in excludes
            .iter()
            .map(String::as_str)
            .chain(defaults.iter().copied())
        {
            anyhow::ensure!(
                !pattern.starts_with('!'),
                "exclude pattern '{pattern}' cannot start with '!'; put a negation in .ripsemignore"
            );
            overrides
                .add(&format!("!{}", glob_text(pattern, cfg!(windows))))
                .with_context(|| format!("invalid exclude pattern '{pattern}'"))?;
        }
        builder.overrides(overrides.build().context("building exclude overrides")?);
    }

    Ok(builder)
}

/// Globs treat a backslash as an escape, but on Windows it is the separator
/// and cannot be part of a name, so `vendor\third_party` means `vendor/third_party`.
/// A leading `./` names the root, where a glob is anchored already, so it is
/// dropped; left in, it would match nothing.
pub(crate) fn glob_text(pattern: &str, windows: bool) -> String {
    let text = match windows && pattern.contains('\\') {
        true => pattern.replace('\\', "/"),
        false => pattern.to_string(),
    };
    text.strip_prefix("./").unwrap_or(&text).to_string()
}

/// Whether a change at any of these paths can alter what [`collect`] returns:
/// the walk reaches the path, or it is gone from a directory the walk reaches.
/// Reads only the directories on the way to each path, so thousands of writes
/// under an ignored `target/` cost a top-level listing. A path outside the root
/// cannot be judged and counts as a change.
pub fn reaches_any(config: &Config, paths: &[PathBuf]) -> bool {
    let root = &config.root;
    if paths.iter().any(|p| !p.starts_with(root)) {
        return true;
    }
    // The one ignore source that lives inside `.git`, and the config, whose
    // excludes and limits decide what the walk keeps.
    let exclude = root.join(".git").join("info").join("exclude");
    if paths.contains(&exclude) || paths.contains(&config.config_path()) {
        return true;
    }
    let targets: Vec<PathBuf> = paths
        .iter()
        .filter(|p| !is_internal(p, root))
        .cloned()
        .collect();
    if targets.is_empty() {
        return false;
    }
    let Ok(mut builder) = builder(config, true) else {
        return true;
    };
    let on_the_way = targets.clone();
    let filter_root = root.clone();
    builder.filter_entry(move |entry| {
        !is_internal(entry.path(), &filter_root)
            && on_the_way.iter().any(|t| t.starts_with(entry.path()))
    });
    let reached: std::collections::HashSet<PathBuf> = builder
        .build()
        .flatten()
        .map(ignore::DirEntry::into_path)
        .collect();
    targets.iter().any(|path| {
        reached.contains(path)
            || (!path.exists() && path.parent().is_some_and(|dir| reached.contains(dir)))
    })
}

/// ripsem's own data and `.git` are never source. Judged on the path below the
/// root, so a repository that lives inside a directory named `.git` or
/// `.ripsem` still indexes.
fn is_internal(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|c| matches!(c.as_os_str().to_str(), Some(".ripsem") | Some(".git")))
}

/// Windows-1252 for the bytes 0x80 to 0x9F, which that code page gives
/// characters where Latin-1 gives controls. The five it leaves unassigned keep
/// the control, as the WHATWG encoding standard has them.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20ac}', '\u{81}', '\u{201a}', '\u{192}', '\u{201e}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{2c6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8d}', '\u{17d}', '\u{8f}',
    '\u{90}', '\u{2018}', '\u{2019}', '\u{201c}', '\u{201d}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{2dc}', '\u{2122}', '\u{161}', '\u{203a}', '\u{153}', '\u{9d}', '\u{17e}', '\u{178}',
];

/// UTF-8 when the bytes are UTF-8. A file that is mostly UTF-8 with a few stray
/// bytes keeps its characters, the stray bytes becoming replacement characters.
/// Otherwise the bytes are read as Windows-1252, which every byte maps to, so a
/// file saved in a Western code page keeps its accented words instead of
/// turning each one into replacement characters.
fn utf8_or_windows_1252(bytes: &[u8]) -> Cow<'_, str> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => {
            let lossy = String::from_utf8_lossy(bytes);
            let stray = lossy.matches('\u{FFFD}').count();
            if stray * 100 <= bytes.len() {
                return Cow::Owned(lossy.into_owned());
            }
            Cow::Owned(
                bytes
                    .iter()
                    .map(|&b| match b {
                        0x80..=0x9F => WINDOWS_1252_HIGH[usize::from(b - 0x80)],
                        _ => char::from(b),
                    })
                    .collect(),
            )
        }
    }
}

/// The text a file holds. UTF-16 and UTF-32 with their byte order mark, as
/// Windows tools save them, are decoded and the mark dropped; anything else is
/// read as UTF-8, or as Windows-1252 when it is not UTF-8, after a UTF-8 mark
/// if there is one. Every reader of a file's text goes through here, so a line
/// number means the same line in the index and in the snippets.
pub fn decode_text(bytes: &[u8]) -> Cow<'_, str> {
    let units = |order: fn([u8; 2]) -> u16, rest: &[u8]| {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| order(pair))
            .collect();
        String::from_utf16_lossy(&units)
    };
    let scalars = |order: fn([u8; 4]) -> u32, rest: &[u8]| {
        rest.as_chunks::<4>()
            .0
            .iter()
            .map(|&quad| char::from_u32(order(quad)).unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect::<String>()
    };
    // The little-endian UTF-32 mark starts with the whole UTF-16 one, so it is
    // tried first.
    match bytes {
        [0xFF, 0xFE, 0, 0, rest @ ..] => Cow::Owned(scalars(u32::from_le_bytes, rest)),
        [0, 0, 0xFE, 0xFF, rest @ ..] => Cow::Owned(scalars(u32::from_be_bytes, rest)),
        [0xFF, 0xFE, rest @ ..] => Cow::Owned(units(u16::from_le_bytes, rest)),
        [0xFE, 0xFF, rest @ ..] => Cow::Owned(units(u16::from_be_bytes, rest)),
        [0xEF, 0xBB, 0xBF, rest @ ..] => utf8_or_windows_1252(rest),
        _ => match utf16_without_mark(&bytes[..bytes.len().min(SNIFF_BYTES as usize)]) {
            Some(Order::Little) => Cow::Owned(units(u16::from_le_bytes, bytes)),
            Some(Order::Big) => Cow::Owned(units(u16::from_be_bytes, bytes)),
            None => utf8_or_windows_1252(bytes),
        },
    }
}

/// The bytes git looks at to call a file binary, which is also enough here.
const SNIFF_BYTES: u64 = 8000;

/// `content_inspector` looks for a NUL only in the first kilobyte, so the rest
/// of the sniffed bytes are checked here. UTF-16 and UTF-32 text, which a byte
/// order mark announces, has NULs by design.
fn reads_as_text(prefix: &[u8]) -> bool {
    match content_inspector::inspect(prefix) {
        content_inspector::ContentType::UTF_16LE
        | content_inspector::ContentType::UTF_16BE
        | content_inspector::ContentType::UTF_32LE
        | content_inspector::ContentType::UTF_32BE => true,
        content_inspector::ContentType::BINARY => {
            !BINARY_HEADERS
                .iter()
                .any(|header| prefix.starts_with(header))
                && utf16_without_mark(prefix).is_some()
        }
        _ => !prefix.contains(&0),
    }
}

/// Formats whose header shows what they are, whatever bytes follow it.
const BINARY_HEADERS: &[&[u8]] = &[b"\x7fELF", b"\x89PNG", b"%PDF", b"MZ", b"PK\x03\x04"];

#[derive(Clone, Copy, PartialEq, Debug)]
enum Order {
    Little,
    Big,
}

/// The byte order of UTF-16 text that has no byte order mark, as Windows tools
/// sometimes write it. ASCII in UTF-16 has a zero beside every character, on
/// the second byte of each unit when little-endian and on the first when
/// big-endian. So the zeros of a sample must sit on one side, and the units
/// decoded that way must be printable: a binary with zeros in every other byte
/// (sixteen-bit samples, say) fails the second test. `None` for anything else,
/// including a sample without a zero.
fn utf16_without_mark(sample: &[u8]) -> Option<Order> {
    let sample = &sample[..sample.len() & !1];
    let units = sample.len() / 2;
    if units < 8 || !sample.contains(&0) {
        return None;
    }
    let (first, second) = sample
        .as_chunks::<2>()
        .0
        .iter()
        .fold((0, 0), |(f, s), pair| {
            (f + usize::from(pair[0] == 0), s + usize::from(pair[1] == 0))
        });
    let (order, zeros, other) = match second >= first {
        true => (Order::Little, second, first),
        false => (Order::Big, first, second),
    };
    if zeros * 10 < units * 4 || other * 20 > units {
        return None;
    }
    let printable = sample
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| match order {
            Order::Little => u16::from_le_bytes([pair[0], pair[1]]),
            Order::Big => u16::from_be_bytes([pair[0], pair[1]]),
        })
        .filter(|&unit| matches!(unit, 0x09 | 0x0A | 0x0D) || unit >= 0x20)
        .count();
    (printable * 100 >= units * 99).then_some(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    fn config_at(root: &Path) -> Config {
        Config {
            root: root.to_path_buf(),
            ..Default::default()
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn collects_text_and_skips_binary() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("blob.bin", "head\0\0\0\u{1}\u{2}tail");
        dir.write("empty.txt", "");

        let names = names(&collect(&config_at(dir.path())).unwrap());
        assert!(names.contains(&"keep.rs".to_string()));
        assert!(!names.contains(&"blob.bin".to_string()));
        assert!(!names.contains(&"empty.txt".to_string()));
    }

    #[test]
    fn files_come_back_in_path_order() {
        let dir = TempDir::new();
        for name in ["z.rs", "b/y.rs", "a/x.rs", "a/b/w.rs", "a-b.rs", "m.rs"] {
            dir.write(name, "fn main() {}\n");
        }
        let collected = collect(&config_at(dir.path())).unwrap();
        let mut sorted = collected.clone();
        sorted.sort();
        assert_eq!(collected, sorted);
        assert_eq!(collected.len(), 6);
    }

    /// Names whose bytes and components order differently: `a-b` sorts before
    /// `a/b` as bytes, but after `a` as a component.
    #[cfg(unix)]
    #[test]
    fn sort_order_is_the_component_order_of_paths() {
        let names = [
            "a-b", "a/b", "a/b/c", "a/bc", "a.b/x", "a/x", "a", "a b/z", "ab", "a/b-c", "a0", "a/0",
        ];
        let mut files: Vec<(PathBuf, FileStat)> = names
            .iter()
            .rev()
            .map(|n| (PathBuf::from(format!("/r/{n}")), FileStat::UNKNOWN))
            .collect();
        sort_by_path(&mut files);
        let mut expected: Vec<PathBuf> = names
            .iter()
            .map(|n| PathBuf::from(format!("/r/{n}")))
            .collect();
        expected.sort();
        let sorted: Vec<PathBuf> = files.into_iter().map(|(path, _)| path).collect();
        assert_eq!(sorted, expected);
    }

    #[test]
    fn skips_git_internals() {
        let dir = TempDir::new();
        dir.write("main.rs", "fn main() {}\n");
        dir.write(".git/config", "[core]\n\trepositoryformatversion = 0\n");
        dir.write(".git/hooks/pre-commit.sample", "#!/bin/sh\nexit 0\n");

        let collected = collect(&config_at(dir.path())).unwrap();
        assert!(
            collected
                .iter()
                .all(|p| !p.components().any(|c| c.as_os_str() == ".git"))
        );
        assert_eq!(collected.len(), 1);
    }

    #[test]
    fn indexes_a_repository_that_lives_under_a_dot_ripsem_directory() {
        let dir = TempDir::new();
        config_at(dir.path()).create_data_dir().unwrap();
        dir.write(".ripsem/corpora/project/main.rs", "fn main() {}\n");
        dir.write(".ripsem/corpora/project/index.bin", "not ripsem's index\n");
        dir.write(".ripsem/corpora/project/notes.tmp", "scratch\n");
        let root = dir.path().join(".ripsem/corpora/project");

        let mut collected = names(&collect(&config_at(&root)).unwrap());
        collected.sort();
        assert_eq!(collected, ["index.bin", "main.rs", "notes.tmp"]);
    }

    #[test]
    fn skips_the_index_directory_inside_the_root() {
        let dir = TempDir::new();
        dir.write("main.rs", "fn main() {}\n");
        dir.write(".ripsem/config.toml", "model = \"x\"\n");
        dir.write(".ripsem/.gitignore", "/index.bin\n");

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["main.rs"]
        );
    }

    #[test]
    fn applies_config_exclude_globs() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("Cargo.lock", "version = 3\n");
        let mut config = config_at(dir.path());
        config.exclude = vec!["*.lock".into()];

        let names = names(&collect(&config).unwrap());
        assert!(names.contains(&"keep.rs".to_string()));
        assert!(!names.contains(&"Cargo.lock".to_string()));
    }

    #[test]
    fn skips_generated_files_without_configuration() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("Cargo.lock", "version = 3\n");
        dir.write("package-lock.json", "{\"lockfileVersion\": 3}\n");
        dir.write("go.sum", "example.com/mod v1.0.0 h1:abc=\n");
        dir.write("app.min.js", "function a(){return 1}\n");
        dir.write("app.js.map", "{\"version\":3}\n");

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["keep.rs"]
        );
    }

    #[test]
    fn generated_files_are_indexed_when_the_default_is_turned_off() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("Cargo.lock", "version = 3\n");
        let mut config = config_at(dir.path());
        config.skip_generated = false;

        assert!(names(&collect(&config).unwrap()).contains(&"Cargo.lock".to_string()));
    }

    #[cfg(windows)]
    #[test]
    fn an_exclude_written_with_backslashes_matches_a_directory() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("vendor/third_party/skip.rs", "fn main() {}\n");
        dir.write("vendor/own.rs", "fn main() {}\n");
        let mut config = config_at(dir.path());
        config.exclude = vec!["vendor\\third_party".into()];

        let mut found = names(&collect(&config).unwrap());
        found.sort();
        assert_eq!(found, ["keep.rs", "own.rs"]);
    }

    #[test]
    fn a_leading_dot_slash_in_an_exclude_names_the_same_path() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("vendor/skip.rs", "fn main() {}\n");
        let mut config = config_at(dir.path());
        config.exclude = vec!["./vendor".into()];

        assert_eq!(names(&collect(&config).unwrap()), ["keep.rs"]);
    }

    #[test]
    fn a_glob_reads_its_separators_as_the_platform_does() {
        assert_eq!(glob_text("vendor\\third_party", true), "vendor/third_party");
        assert_eq!(glob_text("vendor\\", true), "vendor/");
        assert_eq!(glob_text(".\\vendor\\x", true), "vendor/x");
        assert_eq!(glob_text("vendor\\x", false), "vendor\\x");
        assert_eq!(glob_text("./vendor/x", false), "vendor/x");
        assert_eq!(glob_text("./vendor/x", true), "vendor/x");
        assert_eq!(glob_text("src/**/*.rs", true), "src/**/*.rs");
        assert_eq!(glob_text("my dir/a b.rs", true), "my dir/a b.rs");
    }

    #[test]
    fn invalid_exclude_glob_is_an_error() {
        let dir = TempDir::new();
        let mut config = config_at(dir.path());
        config.exclude = vec!["[".into()];
        assert!(collect(&config).is_err());
    }

    #[test]
    fn a_negated_exclude_is_an_error() {
        let dir = TempDir::new();
        let mut config = config_at(dir.path());
        config.exclude = vec!["*.md".into(), "!keep.md".into()];
        let err = format!("{:#}", collect(&config).unwrap_err());
        assert!(err.contains(".ripsemignore"), "{err}");
    }

    #[test]
    fn a_change_the_walk_would_see_reaches_the_index() {
        let dir = TempDir::new();
        dir.write(".gitignore", "target/\n*.log\n");
        dir.write("src/deep/lib.rs", "fn main() {}\n");
        dir.write("target/debug/build.rs", "fn main() {}\n");
        dir.write("debug.log", "noise\n");
        dir.write("Cargo.lock", "version = 3\n");
        dir.write(".git/index", "binary\n");
        let config = config_at(dir.path());
        let reaches = |rel: &str| reaches_any(&config, &[dir.path().join(rel)]);

        assert!(reaches("src/deep/lib.rs"));
        assert!(reaches(".gitignore"), "a new ignore rule changes the walk");
        assert!(
            reaches("src/deep/gone.rs"),
            "removed from a walked directory"
        );
        assert!(reaches("src/new_dir"), "removed or renamed directory");
        assert!(
            reaches(".git/info/exclude"),
            "git's own ignore file changes the walk too"
        );

        assert!(!reaches("target/debug/build.rs"));
        assert!(!reaches("target/debug/gone.rs"));
        assert!(!reaches("debug.log"));
        assert!(!reaches("Cargo.lock"), "generated files are skipped too");
        assert!(!reaches(".git/index"));
        assert!(!reaches(".ripsem/index.bin"));
        assert!(
            reaches(".ripsem/config.toml"),
            "a new exclude or size limit changes the walk"
        );
    }

    #[test]
    fn one_relevant_path_in_a_burst_is_enough() {
        let dir = TempDir::new();
        dir.write(".gitignore", "target/\n");
        dir.write("src/lib.rs", "fn main() {}\n");
        dir.write("target/a.o", "x\n");
        let config = config_at(dir.path());
        let noise = dir.path().join("target/a.o");
        let source = dir.path().join("src/lib.rs");

        assert!(!reaches_any(&config, std::slice::from_ref(&noise)));
        assert!(reaches_any(&config, &[noise, source]));
        assert!(!reaches_any(&config, &[]));
    }

    /// A path that does not start with the root cannot be judged. A needless
    /// reindex is cheaper than a missed change.
    #[test]
    fn a_path_outside_the_root_counts_as_a_change() {
        let dir = TempDir::new();
        let config = config_at(&dir.path().join("repo"));
        assert!(reaches_any(&config, &[dir.path().join("elsewhere.rs")]));
    }

    #[test]
    fn skips_files_over_the_size_limit() {
        let dir = TempDir::new();
        dir.write("big.txt", &"x".repeat(2048));
        let mut config = config_at(dir.path());
        config.max_file_kb = 1;

        assert!(!names(&collect(&config).unwrap()).contains(&"big.txt".to_string()));
    }

    /// Git calls a file binary when a NUL shows up in its first 8000 bytes. One
    /// past the first kilobyte is still binary, not text with garbled words.
    #[test]
    fn a_nul_past_the_first_kilobyte_makes_a_file_binary() {
        let dir = TempDir::new();
        let mut blob = "fn kept() {}\n".repeat(200).into_bytes();
        blob.extend_from_slice(b"\0\x01\x02 binary tail\n");
        assert!(blob.len() < 8000 && blob[1024..].contains(&0));
        std::fs::write(dir.path().join("dump.rs"), &blob).unwrap();
        dir.write("keep.rs", "fn main() {}\n");

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["keep.rs"]
        );
    }

    #[test]
    fn decodes_utf16_with_its_byte_order_mark() {
        let text = "fn a() {}\r\nfn b() {}";
        let le: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let be: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(decode_text(&le), text);
        assert_eq!(decode_text(&be), text);
        assert_eq!(decode_text(b"plain\n"), "plain\n");
    }

    #[test]
    fn decodes_utf32_with_its_byte_order_mark() {
        let text = "fn a() {}\r\nfn b() {} // caf\u{e9} \u{1f980}";
        let le: Vec<u8> = [0xFF, 0xFE, 0, 0]
            .into_iter()
            .chain(text.chars().flat_map(|c| (c as u32).to_le_bytes()))
            .collect();
        let be: Vec<u8> = [0, 0, 0xFE, 0xFF]
            .into_iter()
            .chain(text.chars().flat_map(|c| (c as u32).to_be_bytes()))
            .collect();
        assert_eq!(decode_text(&le), text);
        assert_eq!(decode_text(&be), text);
    }

    #[test]
    fn drops_the_utf8_byte_order_mark() {
        assert_eq!(
            decode_text(b"\xEF\xBB\xBFusing System;\n"),
            "using System;\n"
        );
        assert!(crate::chunk::is_blank(&decode_text(b"\xEF\xBB\xBF\n")));
    }

    /// Source saved by a Windows editor in the system code page is not UTF-8.
    /// Read as UTF-8 it loses every accented word; Windows-1252 reads it as
    /// the characters its author typed.
    #[test]
    fn text_that_is_not_utf8_is_read_as_windows_1252() {
        assert_eq!(
            decode_text(b"caf\xE9 costs \x80 5\x94\n"),
            "caf\u{e9} costs \u{20ac} 5\u{201d}\n"
        );
        assert_eq!(decode_text(b"plain \xC3\xA9\n"), "plain \u{e9}\n");
    }

    #[test]
    fn a_minified_bundle_is_skipped_and_a_long_line_in_a_normal_file_is_kept() {
        let dir = TempDir::new();
        dir.write("bundle.js", &"var a=1;".repeat(2000));
        let mut normal = "fn line() {}\n".repeat(300);
        normal.push_str(&"x".repeat(9000));
        normal.push('\n');
        dir.write("normal.rs", &normal);

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["normal.rs"]
        );
    }

    #[test]
    fn tally_gives_each_left_out_file_the_rule_that_took_it_out() {
        let dir = TempDir::new();
        dir.write("keep.rs", "fn main() {}\n");
        dir.write("bundle.js", &"var a=1;".repeat(1200));
        dir.write("big.rs", &"fn a() {}\n".repeat(2000));
        std::fs::write(dir.path().join("blob.dat"), b"\0\x01\x02 binary\n").unwrap();
        dir.write("generated.rs", "fn g() {}\n");
        let mut config = config_at(dir.path());
        config.max_file_kb = 16;
        config.exclude = vec!["generated.rs".into()];

        let (paths, counts) = tally(&config).unwrap();

        assert_eq!(names(&paths), ["keep.rs"]);
        assert_eq!(
            counts,
            Tally {
                indexed: 1,
                binary: 1,
                minified: 1,
                too_large: 1,
                excluded: 1,
                ..Tally::default()
            }
        );
        assert_eq!(counts.seen(), 5);
    }

    #[test]
    fn utf16_without_a_byte_order_mark_is_text_in_either_order() {
        let text = "fn a() {}\r\nfn b() { \"caf\u{e9}\" }\r\n";
        let le: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let dir = TempDir::new();
        std::fs::write(dir.path().join("le.ps1"), &le).unwrap();
        std::fs::write(dir.path().join("be.reg"), &be).unwrap();

        assert_eq!(names(&collect(&config_at(dir.path())).unwrap()).len(), 2);
        assert_eq!(
            decode_text(&le),
            "fn a() {}\r\nfn b() { \"caf\u{e9}\" }\r\n"
        );
        assert_eq!(
            decode_text(&be),
            "fn a() {}\r\nfn b() { \"caf\u{e9}\" }\r\n"
        );
    }

    #[test]
    fn binary_files_with_a_header_are_still_skipped() {
        let dir = TempDir::new();
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 0, 16, 0, 0, 0, 16, 8, 6, 0, 0, 0]);
        let mut elf = b"\x7fELF\x02\x01\x01\0".to_vec();
        elf.extend([0u8; 56]);
        let mut int16 = Vec::new();
        for n in 0u16..4000 {
            int16.extend((n % 300).to_le_bytes());
        }
        std::fs::write(dir.path().join("icon.png"), &png).unwrap();
        std::fs::write(dir.path().join("prog.elf"), &elf).unwrap();
        std::fs::write(dir.path().join("samples.raw"), &int16).unwrap();
        dir.write("keep.rs", "fn main() {}\n");

        assert_eq!(
            names(&collect(&config_at(dir.path())).unwrap()),
            ["keep.rs"]
        );
    }

    #[test]
    fn a_mostly_utf8_file_with_one_stray_byte_keeps_its_characters() {
        let head = "été, naïve café. ".repeat(60);
        let mut bytes = head.as_bytes().to_vec();
        bytes.extend_from_slice(b"\xE9ok\n");
        assert_eq!(decode_text(&bytes), format!("{head}\u{FFFD}ok\n"));
    }

    #[test]
    fn a_windows_1252_file_is_indexed_with_its_accents() {
        let dir = TempDir::new();
        std::fs::write(dir.path().join("notes.txt"), b"caf\xE9 au lait\n").unwrap();

        let collected = collect(&config_at(dir.path())).unwrap();
        assert_eq!(names(&collected), ["notes.txt"]);
        let text = decode_text(&std::fs::read(&collected[0]).unwrap()).into_owned();
        assert_eq!(text, "caf\u{e9} au lait\n");
    }
}
