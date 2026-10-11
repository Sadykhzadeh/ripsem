use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Result;
use notify::{RecursiveMode, Watcher};

use crate::config::Config;
use crate::{pipeline, walk};

pub fn run(config: Config) -> Result<()> {
    let stats = pipeline::index_repo(&config, false)?;
    eprintln!(
        "indexed {} files ({} chunks); watching for changes",
        stats.files, stats.chunks
    );

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&config.root, RecursiveMode::Recursive)?;

    while let Ok(first) = rx.recv() {
        let mut burst = Burst::default();
        burst.add(first);
        std::thread::sleep(Duration::from_millis(300));
        while let Ok(more) = rx.try_recv() {
            burst.add(more);
        }
        // Writes under an ignored `target/` or inside `.git` cannot change the
        // index, so they should not cost a walk of the whole tree.
        if !burst.rescan && !walk::reaches_any(&config, &burst.paths) {
            continue;
        }
        match pipeline::index_repo(&config, false) {
            Ok(stats) => eprintln!("reindexed: {} chunks", stats.chunks),
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
    fn add(&mut self, event: notify::Result<notify::Event>) {
        match event {
            Ok(event) => {
                self.rescan |= event.need_rescan();
                self.paths.extend(event.paths);
            }
            Err(_) => self.rescan = true,
        }
    }
}
