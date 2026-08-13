use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::Result;
use notify::{RecursiveMode, Watcher};

use crate::config::Config;
use crate::pipeline;

pub fn run(config: Config) -> Result<()> {
    let stats = pipeline::index_repo(&config, false)?;
    eprintln!(
        "indexed {} files ({} chunks); watching for changes",
        stats.files, stats.chunks
    );

    let data_dir = config.data_dir();
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&config.root, RecursiveMode::Recursive)?;

    loop {
        match rx.recv() {
            Ok(Ok(event)) => {
                if event.paths.iter().all(|p| p.starts_with(&data_dir)) {
                    continue;
                }
                drain(&rx);
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
            Ok(Err(_)) => continue,
            Err(_) => break,
        }
    }
    Ok(())
}

fn drain(rx: &std::sync::mpsc::Receiver<notify::Result<notify::Event>>) {
    std::thread::sleep(Duration::from_millis(300));
    while rx.try_recv().is_ok() {}
}
