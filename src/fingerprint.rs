use xxhash_rust::xxh3::xxh3_64;

pub fn content_hash(bytes: &[u8]) -> u64 {
    xxh3_64(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_for_same_input() {
        assert_eq!(content_hash(b"hello world"), content_hash(b"hello world"));
    }

    #[test]
    fn changes_with_input() {
        assert_ne!(content_hash(b"hello world"), content_hash(b"hello worle"));
    }
}
