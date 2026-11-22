use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The files under `root` that differ from `rev` as the working tree has them:
/// the tracked files changed or removed since `rev`, and the untracked ones.
/// Paths are relative to `root` with forward slashes, as the index holds them.
/// Git runs with explicit arguments and no shell.
pub fn changed_since(root: &Path, rev: &str) -> Result<HashSet<String>> {
    if rev.is_empty() || rev.starts_with('-') {
        bail!("'{rev}' is not a revision");
    }
    let dir = crate::cli::show_path(root);
    let probe = Command::new("git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .context("could not run git, which --since needs; is it installed and on PATH?")?;
    anyhow::ensure!(probe.status.success(), "{dir} is not in a git repository");
    git(
        &dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .map_err(|_| anyhow::anyhow!("unknown revision '{rev}'"))?;
    let tracked = git(
        &dir,
        &["diff", "--name-only", "-z", "--relative", rev, "--"],
    )?;
    let untracked = git(&dir, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok(tracked
        .split('\0')
        .chain(untracked.split('\0'))
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

fn git(dir: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("could not run git")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
