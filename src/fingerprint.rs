use std::time::{SystemTime, UNIX_EPOCH};

use bitcode::{Decode, Encode};
use xxhash_rust::xxh3::xxh3_64;

pub fn content_hash(bytes: &[u8]) -> u64 {
    xxh3_64(bytes)
}

/// How long before a walk began a file must have been written for its stat to
/// say anything. Coarse filesystems stamp times in one or two second ticks, so
/// a file written within the tick it was hashed in can change again without
/// its size or time moving.
const RACY_MARGIN_NS: i64 = 2_000_000_000;

/// What a file looked like when it was hashed. An index trusts the hash it
/// stored for a file whose stat is the same again, and reads the file when not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Encode, Decode)]
pub struct FileStat {
    pub size: u64,
    /// Nanoseconds since the Unix epoch.
    pub mtime: i64,
}

impl FileStat {
    /// Equal to nothing a file can have, itself included.
    pub const UNKNOWN: Self = Self {
        size: 0,
        mtime: i64::MIN,
    };

    pub fn of(meta: &std::fs::Metadata) -> Self {
        let mtime = meta.modified().ok().and_then(nanos).unwrap_or(i64::MIN);
        match mtime {
            i64::MIN => Self::UNKNOWN,
            mtime => Self {
                size: meta.len(),
                mtime,
            },
        }
    }

    /// Whether a file now at `current` is the one hashed at `self`.
    pub fn trusts(&self, current: &Self) -> bool {
        self.mtime != i64::MIN && self == current
    }

    /// The stat to store for a file hashed during a walk that began at
    /// `walk_start` ([`now`]). One written within the margin before it, or
    /// after it, may change again unseen, so it is stored as [`Self::UNKNOWN`]
    /// and hashed again next time.
    pub fn settled(self, walk_start: i64) -> Self {
        match self.mtime >= walk_start.saturating_sub(RACY_MARGIN_NS) {
            true => Self::UNKNOWN,
            false => self,
        }
    }
}

fn nanos(time: SystemTime) -> Option<i64> {
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_nanos()).ok()
}

/// The wall clock in the unit of [`FileStat::mtime`].
pub fn now() -> i64 {
    nanos(SystemTime::now()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: i64 = 1_000_000_000;
    const START: i64 = 1_000 * SECOND;

    fn stat(size: u64, mtime: i64) -> FileStat {
        FileStat { size, mtime }
    }

    #[test]
    fn a_file_with_the_stat_it_was_hashed_at_is_trusted() {
        let at = stat(10, START - 60 * SECOND);
        assert!(at.trusts(&at));
        assert!(!at.trusts(&stat(11, at.mtime)), "size differs");
        assert!(!at.trusts(&stat(10, at.mtime + 1)), "mtime differs");
        assert!(!at.trusts(&stat(10, at.mtime - 1)), "mtime moved back");
    }

    #[test]
    fn a_stat_that_could_not_be_read_matches_nothing() {
        let known = stat(10, START - 60 * SECOND);
        assert!(!FileStat::UNKNOWN.trusts(&FileStat::UNKNOWN));
        assert!(!FileStat::UNKNOWN.trusts(&known));
        assert!(!known.trusts(&FileStat::UNKNOWN));
    }

    #[test]
    fn a_file_written_near_the_walk_is_stored_as_unknown() {
        let old = stat(10, START - 3 * SECOND);
        assert_eq!(old.settled(START), old);
        let edge = stat(10, START - 2 * SECOND);
        assert_eq!(
            edge.settled(START),
            FileStat::UNKNOWN,
            "not older than the margin"
        );
        let recent = stat(10, START - SECOND / 2);
        assert_eq!(recent.settled(START), FileStat::UNKNOWN);
        let future = stat(10, START + 90 * SECOND);
        assert_eq!(future.settled(START), FileStat::UNKNOWN);
        assert!(!recent.settled(START).trusts(&recent));
    }

    #[test]
    fn reads_the_modification_time_of_a_file() {
        let dir = crate::testutil::TempDir::new();
        dir.write("a.txt", "abc");
        let stat = FileStat::of(&std::fs::metadata(dir.path().join("a.txt")).unwrap());
        assert_eq!(stat.size, 3);
        assert!(stat.mtime <= now() && stat.mtime > now() - 3_600 * SECOND);
    }

    #[test]
    fn stable_for_same_input() {
        assert_eq!(content_hash(b"hello world"), content_hash(b"hello world"));
    }

    #[test]
    fn changes_with_input() {
        assert_ne!(content_hash(b"hello world"), content_hash(b"hello worle"));
    }
}
