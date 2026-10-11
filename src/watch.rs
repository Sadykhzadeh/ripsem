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
        // A build writing under `target/`, a git command rewriting `.git`, an
        // editor's swap file under an ignored path: none of it can change the
        // index, and each used to cost a full walk and hash of the tree.
        if !burst.rescan && !walk::reaches_any(&config, &burst.paths) {
            continue;
        }
        match pipeline::index_repo(&config, false) {
            Ok(stats) => eprintln!("reindexed: {} chunks", stats.chunks),
            // A reindex can fail for a reason that does not outlive the
            // attempt: a file that vanished between the scan and the
            // read, or a virus scanner holding the index file open as
            // it is replaced. Ending the watch there would leave the
            // index frozen while a session that thinks it is live keeps
            // querying it.
            Err(e) => eprintln!("reindex failed, still watching: {e:#}"),
        }
    }
    Ok(())
}

/// Everything that arrived within one debounce window.
#[derive(Default)]
struct Burst {
    paths: Vec<PathBuf>,
    /// The watcher lost events, or reported an error, so the paths cannot
    /// be trusted to say what changed.
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
