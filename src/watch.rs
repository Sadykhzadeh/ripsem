use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Result;
use notify::{EventKind, RecursiveMode, Watcher};

use crate::cli::plural;
use crate::config::Config;
use crate::{pipeline, walk};

pub fn run(mut config: Config) -> Result<()> {
    // Watching starts first, so an edit made while the first index is built
    // still reaches it.
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&config.root, RecursiveMode::Recursive)?;

    let stats = pipeline::index_repo(&config, false)?;
    eprintln!(
        "indexed {} ({}); watching for changes",
        plural(stats.files, "file"),
        plural(stats.chunks, "chunk")
    );

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
            Err(e) => eprintln!("config unreadable, keeping the previous one: {e:#}"),
        }
        // Writes under an ignored `target/` or inside `.git` cannot change the
        // index, so they should not cost a walk of the whole tree.
        if !burst.rescan && !walk::reaches_any(&config, &burst.paths) {
            continue;
        }
        match pipeline::index_repo(&config, false) {
            Ok(stats) => eprintln!("reindexed: {}", plural(stats.chunks, "chunk")),
            // Failures are often transient (a file removed mid-scan, a scanner
            // holding the index open). Quitting would leave the index frozen
            // while a running session keeps querying it.
            Err(e) => eprintln!("reindex failed, still watching: {e:#}"),
        }
    }
    Ok(())
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
                self.paths.extend(event.paths);
            }
            Err(_) => self.rescan = true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{AccessKind, AccessMode, DataChange, ModifyKind};

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
}
