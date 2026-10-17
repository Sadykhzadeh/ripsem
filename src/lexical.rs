use bitcode::{Decode, Encode};
use xxhash_rust::xxh3::xxh3_64;

pub const MAX_QUERY_TERMS: usize = 64;

const K1: f32 = 1.2;
const B: f32 = 0.75;
const MIN_TOKEN_BYTES: usize = 2;
const MIN_NAME_BYTES: usize = 3;
const NAME_WORDS: usize = 3;

pub type TermHash = u64;

fn document_terms(text: &str, path: &str) -> Vec<TermHash> {
    let mut out = Vec::new();
    let mut buffer = String::new();
    collect_terms(text, &mut buffer, &mut out);
    collect_terms(path, &mut buffer, &mut out);
    out
}

pub fn query_terms(text: &str) -> Vec<TermHash> {
    let mut out = Vec::new();
    let mut buffer = String::new();
    collect_terms(text, &mut buffer, &mut out);
    // A query past the cap, such as a pasted stack trace, keeps the terms it
    // opens with rather than whichever happen to hash lowest.
    let mut seen = std::collections::HashSet::new();
    out.retain(|term| seen.insert(*term));
    out.truncate(MAX_QUERY_TERMS);
    out
}

/// A name as it is remembered rather than typed. Case and separators are
/// dropped, so `SPEED_LIMIT`, `speedLimit` and the words "speed limit" agree.
pub fn name_key(name: &str) -> Option<TermHash> {
    let folded: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    (folded.len() >= MIN_NAME_BYTES).then(|| xxh3_64(folded.as_bytes()))
}

/// The names a query may be asking for, as [`name_key`]s. A word written as
/// code (`parse_header`, `QuantizedVector`, `Searcher::refresh`) names
/// something on its own, and so does a query of one word. A plain word inside a
/// sentence does not, or every "request" in a question would call each method
/// named `request` a definition of it; two or three of them in a row can spell
/// a name out ("cache error" for `CacheError`).
pub fn query_names(text: &str) -> Vec<TermHash> {
    let words: Vec<&str> = text
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .take(MAX_QUERY_TERMS)
        .collect();
    let mut out = Vec::new();
    let mut joined = String::new();
    for (start, word) in words.iter().enumerate() {
        if words.len() == 1 || written_as_code(text, word, start) {
            out.extend(name_key(word));
        }
        joined.clear();
        joined.push_str(word);
        for next in words[start + 1..].iter().take(NAME_WORDS - 1) {
            joined.push_str(next);
            out.extend(name_key(&joined));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// A capital that opens the query only starts a sentence, and a full stop
/// or comma after a word only ends a clause.
fn written_as_code(text: &str, word: &str, position: usize) -> bool {
    let mut chars = word.chars();
    let first_upper = chars.next().is_some_and(char::is_uppercase);
    split_identifier(word).len() > 1
        || chars.any(char::is_uppercase)
        || (first_upper && position > 0)
        || text.split_whitespace().any(|token| {
            let token = token.trim_end_matches(['.', ',', ';', ':', '!', '?', '"', '\'']);
            token.contains(word)
                && (token.contains('`')
                    || (token.len() > word.len()
                        && token.contains([':', '.', '(', '#', '<', '$', '@'])))
        })
}

fn collect_terms(text: &str, buffer: &mut String, out: &mut Vec<TermHash>) {
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.is_empty() {
            continue;
        }
        push_term(word, buffer, out);
        let pieces = split_identifier(word);
        if pieces.len() > 1 {
            for piece in pieces {
                push_term(piece, buffer, out);
            }
        }
    }
}

fn push_term(word: &str, buffer: &mut String, out: &mut Vec<TermHash>) {
    if word.len() < MIN_TOKEN_BYTES {
        return;
    }
    buffer.clear();
    buffer.extend(word.chars().flat_map(char::to_lowercase));
    out.push(xxh3_64(buffer.as_bytes()));
}

fn split_identifier(word: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for segment in word.split('_').filter(|s| !s.is_empty()) {
        let bytes = segment.as_bytes();
        let mut start = 0;
        for i in 1..bytes.len() {
            if starts_new_piece(bytes, i) {
                out.push(&segment[start..i]);
                start = i;
            }
        }
        out.push(&segment[start..]);
    }
    out
}

fn starts_new_piece(bytes: &[u8], i: usize) -> bool {
    let previous = bytes[i - 1];
    let current = bytes[i];
    if (previous.is_ascii_lowercase() || previous.is_ascii_digit()) && current.is_ascii_uppercase()
    {
        return true;
    }
    if previous.is_ascii_uppercase() && current.is_ascii_uppercase() {
        return bytes.get(i + 1).is_some_and(u8::is_ascii_lowercase);
    }
    false
}

#[derive(Debug, Clone, Default, Encode, Decode)]
pub struct TermVector {
    terms: Vec<(TermHash, u8)>,
    length: u32,
}

impl TermVector {
    /// The declared name is added as a term of its own: a fragment of a large
    /// function does not contain the function's name, and could not be found by
    /// it otherwise.
    pub fn new(text: &str, path: &str, symbol: Option<&str>) -> Self {
        let mut hashes = document_terms(text, path);
        if let Some(name) = symbol {
            let mut buffer = String::new();
            collect_terms(name, &mut buffer, &mut hashes);
        }
        let length = hashes.len() as u32;
        hashes.sort_unstable();
        let mut terms: Vec<(TermHash, u8)> = Vec::with_capacity(hashes.len());
        for hash in hashes {
            match terms.last_mut() {
                Some(last) if last.0 == hash => last.1 = last.1.saturating_add(1),
                _ => terms.push((hash, 1)),
            }
        }
        terms.shrink_to_fit();
        Self {
            terms,
            length: length.max(1),
        }
    }

    pub fn length(&self) -> u32 {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn frequency(&self, term: TermHash) -> Option<u8> {
        self.terms
            .binary_search_by_key(&term, |&(hash, _)| hash)
            .ok()
            .map(|i| self.terms[i].1)
    }

    pub fn presence(&self, terms: &[TermHash]) -> u64 {
        let mut bits = 0u64;
        for (i, &term) in terms.iter().enumerate() {
            if self.frequency(term).is_some() {
                bits |= 1 << i;
            }
        }
        bits
    }
}

pub struct Weights {
    idf: Vec<f32>,
    reachable_idf: f32,
    total_idf: f32,
    average_length: f32,
}

impl Weights {
    pub fn new(presence: &[u64], term_count: usize, average_length: f32) -> Self {
        let corpus_size = presence.len();
        let mut frequencies = vec![0usize; term_count];
        for bits in presence {
            let mut rest = *bits;
            while rest != 0 {
                frequencies[rest.trailing_zeros() as usize] += 1;
                rest &= rest - 1;
            }
        }
        let idf: Vec<f32> = frequencies
            .iter()
            .map(|&df| inverse_document_frequency(df, corpus_size))
            .collect();
        let reachable_idf = idf
            .iter()
            .zip(&frequencies)
            .filter(|&(_, &df)| df > 0)
            .map(|(weight, _)| weight)
            .sum();
        Self {
            total_idf: idf.iter().sum(),
            idf,
            reachable_idf,
            average_length: average_length.max(1.0),
        }
    }

    pub fn bm25(&self, terms: &[TermHash], vector: &TermVector, presence: u64) -> f32 {
        let normalization = K1 * (1.0 - B + B * vector.length() as f32 / self.average_length);
        let mut score = 0.0;
        for i in set_bits(presence) {
            let frequency = vector.frequency(terms[i]).unwrap_or(1) as f32;
            score += self.idf[i] * frequency * (K1 + 1.0) / (frequency + normalization);
        }
        score
    }

    pub fn coverage(&self, presence: u64) -> f32 {
        // A record with no query term returns +0.0 here, since an empty f32
        // sum is -0.0 and that sign would reach the score.
        if self.reachable_idf <= 0.0 || presence == 0 {
            return 0.0;
        }
        let matched: f32 = set_bits(presence).map(|i| self.idf[i]).sum();
        (matched / self.reachable_idf).min(1.0)
    }

    /// The share of the whole query found literally, which is what a score
    /// reports. [`Self::coverage`] leaves out terms no record holds, so that a
    /// misremembered name still ranks the code it meant, but reporting that
    /// would call a chunk sharing one word with a deleted identifier certain.
    pub fn share(&self, presence: u64) -> f32 {
        if self.total_idf <= 0.0 || presence == 0 {
            return 0.0;
        }
        let matched: f32 = set_bits(presence).map(|i| self.idf[i]).sum();
        (matched / self.total_idf).min(1.0)
    }
}

fn set_bits(mut bits: u64) -> impl Iterator<Item = usize> {
    std::iter::from_fn(move || {
        if bits == 0 {
            return None;
        }
        let i = bits.trailing_zeros() as usize;
        bits &= bits - 1;
        Some(i)
    })
}

fn inverse_document_frequency(df: usize, corpus_size: usize) -> f32 {
    let n = corpus_size as f32;
    let d = df as f32;
    (1.0 + (n - d + 0.5) / (d + 0.5)).ln()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms_of(text: &str) -> Vec<TermHash> {
        query_terms(text)
    }

    fn contains(text: &str, token: &str) -> bool {
        terms_of(text).contains(&terms_of(token)[0])
    }

    fn names(query: &str) -> Vec<TermHash> {
        query_names(query)
    }

    fn named(query: &str, name: &str) -> bool {
        names(query).contains(&name_key(name).unwrap())
    }

    #[test]
    fn a_name_matches_however_it_is_cased_or_separated() {
        assert_eq!(name_key("SPEED_LIMIT"), name_key("speedLimit"));
        assert_eq!(name_key("speed-limit"), name_key("SpeedLimit"));
        assert_eq!(name_key("ab"), None);
    }

    #[test]
    fn a_word_written_as_code_names_a_declaration() {
        assert!(named("where is parse_header called", "parse_header"));
        assert!(named("QuantizedVector", "QuantizedVector"));
        assert!(named("what does Searcher::refresh do", "refresh"));
        assert!(named("fn quote_cost", "quote_cost"));
        assert!(named("where is Router defined", "Router"));
        assert!(named("what calls `refresh` here", "refresh"));
        assert!(named("who calls refresh() first", "refresh"));
        assert!(named("request", "request"));
    }

    #[test]
    fn plain_words_in_a_sentence_name_nothing_alone() {
        assert!(!named("abort a request when it takes too long", "request"));
        assert!(!named("Match an incoming request", "Match"));
        assert!(!named("keep the index up to date", "index"));
        assert!(!named("abort a slow url request.", "request"));
        assert!(!named("where is the timeout set, and how long", "set"));
    }

    #[test]
    fn words_in_a_row_spell_a_name_out() {
        assert!(named(
            "where is the lane speed limit defined",
            "LANE_SPEED_LIMIT"
        ));
        assert!(named("where is the lane speed limit defined", "SpeedLimit"));
        assert!(named("cache error type", "CacheError"));
        assert!(!named("cache error type", "CacheErrorTypeKind"));
    }

    #[test]
    fn a_long_query_keeps_the_terms_it_opens_with() {
        let words: Vec<String> = (0..100).map(|i| format!("word{i}")).collect();
        let kept: Vec<TermHash> = words[..MAX_QUERY_TERMS]
            .iter()
            .map(|w| terms_of(w)[0])
            .collect();
        assert_eq!(terms_of(&words.join(" ")), kept);
    }

    #[test]
    fn keeps_the_whole_identifier_and_its_pieces() {
        assert!(contains("fn parse_header()", "parse_header"));
        assert!(contains("fn parse_header()", "parse"));
        assert!(contains("fn parse_header()", "header"));
    }

    #[test]
    fn splits_camel_case_including_acronyms() {
        assert!(contains("parseHeader", "parse"));
        assert!(contains("parseHeader", "header"));
        assert!(contains("HTTPServer", "http"));
        assert!(contains("HTTPServer", "server"));
        assert!(contains("HTTPServer", "httpserver"));
    }

    #[test]
    fn keeps_digits_attached_to_their_word() {
        assert!(contains("bge_small_en_v1", "v1"));
        assert!(contains("utf8Decode", "utf8"));
        assert!(contains("utf8Decode", "decode"));
    }

    #[test]
    fn drops_single_character_tokens() {
        assert!(terms_of("a b c").is_empty());
    }

    #[test]
    fn query_terms_are_deduplicated_and_capped() {
        assert_eq!(terms_of("cache CACHE cache").len(), 1);
        let long: String = (0..200).map(|i| format!("token{i} ")).collect();
        assert_eq!(terms_of(&long).len(), MAX_QUERY_TERMS);
    }

    #[test]
    fn path_terms_are_searchable() {
        let vector = TermVector::new("fn nothing() {}", "src/embed/fastembed.rs", None);
        assert!(vector.presence(&terms_of("fastembed")) != 0);
        assert!(vector.presence(&terms_of("embed")) != 0);
    }

    /// Pieces of a function over the chunk budget never mention the function;
    /// the declared name keeps them reachable by it.
    #[test]
    fn a_fragment_is_reachable_by_the_name_of_its_function() {
        let terms = terms_of("evict_least_recently_used");
        let fragment = TermVector::new(
            "map.remove(oldest);",
            "cache.rs",
            Some("evict_least_recently_used"),
        );
        let anonymous = TermVector::new("map.remove(oldest);", "cache.rs", None);
        assert_eq!(fragment.presence(&terms).count_ones() as usize, terms.len());
        assert_eq!(anonymous.presence(&terms), 0);
    }

    #[test]
    fn repeated_terms_accumulate_frequency() {
        let vector = TermVector::new("cache cache cache", "x.rs", None);
        let cache = terms_of("cache")[0];
        assert_eq!(vector.frequency(cache), Some(3));
    }

    #[test]
    fn presence_marks_the_query_term_positions() {
        let vector = TermVector::new("fn evict_least_recently_used() {}", "cache.rs", None);
        let terms = terms_of("evict_least_recently_used");
        let bits = vector.presence(&terms);
        assert_eq!(bits.count_ones() as usize, terms.len());
        assert!(
            TermVector::new("fn unrelated() {}", "other.rs", None)
                .presence(&terms)
                .count_ones()
                < terms.len() as u32
        );
    }

    fn corpus() -> Vec<TermVector> {
        vec![
            TermVector::new("fn evict_least_recently_used(cache) {}", "cache.rs", None),
            TermVector::new("fn sign_in(name, secret) {}", "auth.rs", None),
            TermVector::new("fn rectangle_area(w, h) {}", "geometry.rs", None),
        ]
    }

    fn scored(query: &str) -> Vec<(usize, f32, f32)> {
        let terms = terms_of(query);
        let vectors = corpus();
        let presence: Vec<u64> = vectors.iter().map(|v| v.presence(&terms)).collect();
        let average = vectors.iter().map(|v| v.length() as f32).sum::<f32>() / vectors.len() as f32;
        let weights = Weights::new(&presence, terms.len(), average);
        presence
            .iter()
            .enumerate()
            .map(|(i, &bits)| {
                (
                    i,
                    weights.bm25(&terms, &vectors[i], bits),
                    weights.coverage(bits),
                )
            })
            .collect()
    }

    #[test]
    fn an_exact_identifier_outscores_everything_else() {
        let rows = scored("evict_least_recently_used");
        assert!(rows[0].1 > rows[1].1);
        assert!(rows[0].1 > rows[2].1);
        assert_eq!(rows[0].2, 1.0);
    }

    #[test]
    fn coverage_ignores_terms_absent_from_the_corpus() {
        let rows = scored("evict_least_recently_used zzzznotpresent");
        assert_eq!(rows[0].2, 1.0);
    }

    #[test]
    fn the_reported_share_counts_terms_absent_from_the_corpus() {
        let terms = terms_of("evict_least_recently_used zzzznotpresent");
        let vectors = corpus();
        let presence: Vec<u64> = vectors.iter().map(|v| v.presence(&terms)).collect();
        let weights = Weights::new(&presence, terms.len(), 1.0);
        let share = weights.share(presence[0]);
        assert!(share > 0.5 && share < 1.0, "{share}");

        let terms = terms_of("evict_least_recently_used");
        let presence: Vec<u64> = vectors.iter().map(|v| v.presence(&terms)).collect();
        let weights = Weights::new(&presence, terms.len(), 1.0);
        assert_eq!(weights.share(presence[0]), 1.0);
    }

    #[test]
    fn a_rare_term_outweighs_a_common_one() {
        let common = scored("cache");
        let rare = scored("rectangle_area");
        assert!(rare[2].1 > common[0].1);
    }

    #[test]
    fn a_record_without_query_terms_scores_zero() {
        let rows = scored("evict_least_recently_used");
        assert_eq!(rows[1].1, 0.0);
        assert_eq!(rows[1].2, 0.0);
        assert!(
            rows[1].2.is_sign_positive(),
            "a score of -0.0 prints as -0.000"
        );
    }
}
