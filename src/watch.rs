use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Result;
use notify::event::ModifyKind;
use notify::{EventKind, RecursiveMode, Watcher};

use crate::cli::plural;
use crate::config::Config;
use crate::pipeline::{self, IndexStats};
use crate::walk;

static INDEXING: AtomicBool = AtomicBool::new(false);

/// Whether this process is building or updating the index right now.
pub fn indexing() -> bool {
    INDEXING.load(Ordering::Relaxed)
}

pub fn run(config: Config) -> Result<()> {
    keep_current(config, true, |line| eprintln!("{line}"))
}

/// Keeps the index current from a thread of its own, for `ripsem mcp`: an
/// agent edits files all day, and a server that only reads the index would
/// answer from code that no longer exists.
///
/// The first index counts as underway from here, not from the thread's first
/// walk, so a search that arrives while the watcher is being set up is told the
/// index is on its way instead of that there is none.
pub fn spawn(config: Config) {
    INDEXING.store(true, Ordering::Relaxed);
    std::thread::spawn(move || {
        if let Err(e) = keep_current(config, false, |line| tracing::info!("{line}")) {
            tracing::warn!("stopped keeping the index current: {e:#}");
        }
        // A watch that never got as far as an index must not leave searches
        // waiting for one.
        INDEXING.store(false, Ordering::Relaxed);
    });
}

/// `strict` ends the watch when the first index fails, as a command run by hand
/// should, where a server answering an agent keeps watching and retries on the
/// next change, as it does after any later failure.
fn keep_current(mut config: Config, strict: bool, report: impl Fn(String)) -> Result<()> {
    // Watching starts first, so an edit made while the first index is built
    // still reaches it.
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&config.root, RecursiveMode::Recursive)?;

    match index(&config) {
        Ok(stats) => report(format!(
            "indexed {} ({}); watching for changes",
            plural(stats.files, "file"),
            plural(stats.chunks, "chunk")
        )),
        Err(e) if strict => return Err(e),
        Err(e) => report(format!("indexing failed, still watching: {e:#}")),
    }

    while let Ok(first) = rx.recv() {
        let mut burst = Burst::default();
        burst.add(first);
        std::thread::sleep(Duration::from_millis(300));
        while let Ok(more) = rx.try_recv() {
            burst.add(more);
        }
        // `ripsem model set` and a rebuild may have happened since the last
        // burst. Indexing under the old settings would overwrite that index.
        match Config::load(&config.root) {
            Ok(fresh) => config = fresh,
            Err(e) => report(format!(
                "config unreadable, keeping the previous one: {e:#}"
            )),
        }
        // Writes under an ignored `target/` or inside `.git` cannot change the
        // index, so they should not cost a walk of the whole tree.
        if !burst.rescan && !walk::reaches_any(&config, &burst.paths) {
            continue;
        }
        match index(&config) {
            Ok(stats) => report(format!("reindexed: {}", plural(stats.chunks, "chunk"))),
            // Failures are often transient (a file removed mid-scan, a scanner
            // holding the index open). Quitting would leave the index frozen
            // while a running session keeps querying it.
            Err(e) => report(format!("reindex failed, still watching: {e:#}")),
        }
    }
    Ok(())
}

fn index(config: &Config) -> Result<IndexStats> {
    INDEXING.store(true, Ordering::Relaxed);
    let result = pipeline::index_repo(config, false);
    INDEXING.store(false, Ordering::Relaxed);
    result
}

/// Everything that arrived within one debounce window.
#[derive(Default)]
struct Burst {
    paths: Vec<PathBuf>,
    /// The watcher lost events or reported an error, so `paths` is incomplete.
    rescan: bool,
}

impl Burst {
    /// Reading a file changes nothing. inotify reports every open, and a
    /// reindex opens every file, so counting reads would set off the next
    /// reindex forever.
    fn add(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => {}
            Ok(event) => {
                self.rescan |= event.need_rescan();
                // Windows reports a directory as modified, after the event for
                // the file in it, whenever an entry is added or removed. The
                // entry has its own event; a rename of the directory does not
                // come as this kind.
                let touched = matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Any | ModifyKind::Metadata(_))
                );
                self.paths
                    .extend(event.paths.into_iter().filter(|p| !(touched && p.is_dir())));
            }
            Err(_) => self.rescan = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{
        AccessKind, AccessMode, CreateKind, DataChange, MetadataKind, ModifyKind, RenameMode,
    };

    #[test]
    fn reading_a_file_is_not_a_change() {
        let mut burst = Burst::default();
        for access in [
            AccessKind::Open(AccessMode::Any),
            AccessKind::Close(AccessMode::Read),
        ] {
            burst.add(Ok(
                Event::new(EventKind::Access(access)).add_path("a.rs".into())
            ));
        }
        assert!(burst.paths.is_empty() && !burst.rescan);

        burst.add(Ok(Event::new(EventKind::Modify(ModifyKind::Data(
            DataChange::Content,
        )))
        .add_path("a.rs".into())));
        assert_eq!(burst.paths, [PathBuf::from("a.rs")]);
    }

    #[test]
    fn a_directory_whose_entries_changed_is_not_a_change_of_its_own() {
        let dir = crate::testutil::TempDir::new();
        dir.write("src/a.rs", "fn a() {}");
        let src = dir.path().join("src");
        let on = |kind: ModifyKind, path: &PathBuf| {
            let mut burst = Burst::default();
            burst.add(Ok(
                Event::new(EventKind::Modify(kind)).add_path(path.clone())
            ));
            burst.paths
        };
        assert!(on(ModifyKind::Any, &src).is_empty());
        assert!(on(ModifyKind::Metadata(MetadataKind::Any), &src).is_empty());
        assert_eq!(on(ModifyKind::Any, &src.join("a.rs")), [src.join("a.rs")]);
        assert_eq!(
            on(ModifyKind::Name(RenameMode::Any), &src),
            vec![src.clone()]
        );

        let mut burst = Burst::default();
        burst.add(Ok(
            Event::new(EventKind::Create(CreateKind::Folder)).add_path(src.clone())
        ));
        assert_eq!(burst.paths, [src]);
    }

    /// A search that arrives while the watcher is still being set up must be
    /// told the index is on its way, not that there is none.
    #[test]
    fn the_first_index_counts_as_underway_from_the_moment_the_watch_starts() {
        let dir = crate::testutil::TempDir::new();
        dir.write("a.rs", "fn a() {}\n");
        let config = Config {
            root: dir.path().to_path_buf(),
            backend: crate::config::Backend::Hash,
            ..Default::default()
        };
        config.create_data_dir().unwrap();

        spawn(config);
        assert!(indexing());

        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while indexing() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!indexing(), "the flag has to clear once the index is built");
    }

    /// A scanner holding the index file open fails a save, and the server
    /// that watches for an agent's edits must not stop watching for it.
    #[test]
    fn a_first_index_that_fails_does_not_end_the_watch() {
        let dir = crate::testutil::TempDir::new();
        dir.write("a.rs", "fn a() {}\n");
        let config = Config {
            root: dir.path().to_path_buf(),
            backend: crate::config::Backend::Hash,
            ..Default::default()
        };
        config.create_data_dir().unwrap();
        // A rename cannot replace a directory, so saving the index fails.
        std::fs::create_dir(config.index_path()).unwrap();

        let (tx, reports) = channel();
        std::thread::spawn(move || {
            let _ = keep_current(config, false, |line| {
                let _ = tx.send(line);
            });
        });
        let wait = Duration::from_secs(20);
        let first = reports.recv_timeout(wait).expect("a report of the failure");
        assert!(first.contains("failed"), "{first}");

        std::fs::remove_dir(dir.path().join(".ripsem/index.bin")).unwrap();
        dir.write("b.rs", "fn b() {}\n");
        let next = reports.recv_timeout(wait).expect("a report of the reindex");
        assert!(next.starts_with("reindexed"), "{next}");
        assert!(dir.path().join(".ripsem/index.bin").is_file());
    }
}
