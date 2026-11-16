use std::cell::Cell;
use std::sync::Once;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};

use instant_distance::{Builder, HnswMap, Point, Search};
use serde::{Deserialize, Serialize};

/// The lexical arm scans every chunk regardless, so the graph can only save the
/// vector half of a query, and it is rebuilt on every changing reindex. Measured,
/// the flat scan stays as cheap as the graph past 200 000 chunks.
#[cfg(not(test))]
pub const MIN_RECORDS: usize = 500_000;

/// Low enough that a few dozen small files build a graph in the unit tests.
#[cfg(test)]
pub const MIN_RECORDS: usize = 32;

#[derive(Clone, Serialize, Deserialize)]
pub struct Embedding(pub Vec<f32>);

impl Point for Embedding {
    fn distance(&self, other: &Self) -> f32 {
        1.0 - crate::quant::dot_f32(&self.0, &other.0)
    }
}

pub fn build(vectors: Vec<Vec<f32>>) -> Option<Vec<u8>> {
    if vectors.is_empty() {
        return None;
    }
    let values: Vec<u32> = (0..vectors.len() as u32).collect();
    let points: Vec<Embedding> = vectors.into_iter().map(Embedding).collect();
    let map = Builder::default().seed(1).build(points, values);
    bitcode::serialize(&map).ok()
}

pub fn load(blob: &[u8]) -> Option<HnswMap<Embedding, u32>> {
    bitcode::deserialize(blob).ok()
}

thread_local! {
    static CONTAINED: Cell<bool> = const { Cell::new(false) };
}

static HOOK: Once = Once::new();

/// Installs, once, a hook that stays silent for a panic raised while
/// `search_map` contains it on this thread, and passes every other panic to the
/// hook that was there before. Panics elsewhere are reported as they were.
fn silence_contained_panics() {
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !CONTAINED.with(Cell::get) {
                previous(info);
            }
        }));
    });
}

/// `None` when the graph is inconsistent: a blob can decode and still point past
/// its own nodes, which makes the search panic. The search is contained so the
/// caller can fall back to the exact scan, as it does for a blob that does not
/// decode. `Search` is made per call and the map is only read, so nothing is
/// left half-updated.
pub fn search_map(
    map: &HnswMap<Embedding, u32>,
    query: &[f32],
    k: usize,
) -> Option<Vec<(usize, f32)>> {
    silence_contained_panics();
    CONTAINED.with(|c| c.set(true));
    let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut search = Search::default();
        let point = Embedding(query.to_vec());
        map.search(&point, &mut search)
            .take(k)
            .map(|item| (*item.value as usize, 1.0 - item.distance))
            .collect()
    }));
    CONTAINED.with(|c| c.set(false));
    found.ok()
}

/// A decoded graph that remembers when a search failed on it. Once one search
/// has failed, the graph is not entered again by this process: every later
/// query gets `None` at once and the caller takes the exact scan.
pub struct Graph {
    map: HnswMap<Embedding, u32>,
    broken: AtomicBool,
    #[cfg(test)]
    entered: AtomicUsize,
}

impl Graph {
    pub fn new(map: HnswMap<Embedding, u32>) -> Self {
        Self {
            map,
            broken: AtomicBool::new(false),
            #[cfg(test)]
            entered: AtomicUsize::new(0),
        }
    }

    pub fn search(&self, query: &[f32], k: usize) -> Option<Vec<(usize, f32)>> {
        if self.broken.load(Ordering::Relaxed) {
            return None;
        }
        #[cfg(test)]
        self.entered.fetch_add(1, Ordering::Relaxed);
        let found = search_map(&self.map, query, k);
        if found.is_none() && !self.broken.swap(true, Ordering::Relaxed) {
            eprintln!(
                "warning: the nearest-neighbour graph in the index is damaged; \
                 searching every chunk instead. Run `ripsem index --force` to rebuild it"
            );
        }
        found
    }

    #[cfg(test)]
    pub(crate) fn entered(&self) -> usize {
        self.entered.load(Ordering::Relaxed)
    }
}

pub fn search(blob: &[u8], query: &[f32], k: usize) -> Option<Vec<(usize, f32)>> {
    let map = load(blob)?;
    search_map(&map, query, k)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn unit(dims: usize, hot: usize) -> Vec<f32> {
        let mut v = vec![0.0; dims];
        v[hot] = 1.0;
        v
    }

    #[test]
    fn finds_the_nearest_vector() {
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| unit(40, i)).collect();
        let blob = build(vectors).unwrap();

        let hits = search(&blob, &unit(40, 7), 3).unwrap();
        assert_eq!(hits[0].0, 7);
        assert!(hits[0].1 > 0.9);
    }

    #[test]
    fn empty_input_builds_nothing() {
        assert!(build(Vec::new()).is_none());
    }

    #[test]
    fn load_and_search_map_match_search() {
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| unit(40, i)).collect();
        let blob = build(vectors).unwrap();

        let map = load(&blob).unwrap();
        let hits = search_map(&map, &unit(40, 7), 3).unwrap();
        assert_eq!(hits[0].0, 7);
        assert!(hits[0].1 > 0.9);

        // search() wrapper must produce the same top result
        let hits2 = search(&blob, &unit(40, 7), 3).unwrap();
        assert_eq!(hits[0].0, hits2[0].0);
    }

    /// A blob that decodes and panics when searched: the first window of eight
    /// bytes set to all ones that makes a decodable graph fail.
    pub(crate) fn a_graph_blob_that_panics_on_search() -> Vec<u8> {
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| unit(40, i)).collect();
        let blob = build(vectors).unwrap();
        let query = unit(40, 3);
        for width in [1, 2, 4, 8] {
            for pattern in [0xff_u8, 0x7f, 0x80, 0x40] {
                for at in 0..blob.len() - width {
                    let mut bytes = blob.clone();
                    bytes[at..at + width].fill(pattern);
                    let panics =
                        load(&bytes).is_some_and(|map| search_map(&map, &query, 3).is_none());
                    if panics {
                        return bytes;
                    }
                }
            }
        }
        panic!("no window of the graph blob makes a decodable graph panic");
    }

    #[test]
    fn a_graph_that_fails_once_is_not_entered_again() {
        let blob = a_graph_blob_that_panics_on_search();
        let graph = Graph::new(load(&blob).unwrap());
        let query = unit(40, 3);

        assert!(graph.search(&query, 3).is_none());
        assert!(graph.search(&query, 3).is_none());
        assert_eq!(graph.entered(), 1);
    }

    #[test]
    fn a_corrupt_graph_blob_never_panics_or_over_allocates() {
        let vectors: Vec<Vec<f32>> = (0..40).map(|i| unit(40, i)).collect();
        let blob = build(vectors).unwrap();
        let query = unit(40, 3);
        let mut state = 0x853c_49e6_748f_ea9b_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };

        for cut in 0..blob.len() {
            let _ = search(&blob[..cut], &query, 3);
        }
        for _ in 0..2000 {
            let mut bytes = blob.clone();
            for _ in 0..1 + next() % 4 {
                let at = (next() % bytes.len() as u64) as usize;
                bytes[at] = next() as u8;
            }
            let _ = search(&bytes, &query, 3);
        }
        for at in 0..blob.len().saturating_sub(7) {
            let mut bytes = blob.clone();
            bytes[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
            let _ = search(&bytes, &query, 3);
        }
    }
}
