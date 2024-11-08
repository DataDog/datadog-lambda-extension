#![allow(clippy::module_name_repetitions)]

use nix::sys::statfs::statfs;
use std::io;
use std::path::Path;

#[cfg(not(target_os = "windows"))]
pub fn statfs_info(path: &str) -> Result<(f64, f64, f64), io::Error> {
    let stat = statfs(Path::new(path)).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    Ok((
        stat.block_size() as f64,
        stat.blocks() as f64,
        stat.blocks_available() as f64,
    ))
}

#[cfg(target_os = "windows")]
fn statfs_info(path: &str) -> Result<(f64, f64, f64), io::Error> {
    Ok((0, 0, 0))
}
