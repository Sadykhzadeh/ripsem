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
                let stats = pipeline::index_repo(&config, false)?;
                eprintln!("reindexed: {} chunks", stats.chunks);
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
