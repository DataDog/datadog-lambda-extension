#![allow(clippy::module_name_repetitions)]

use nix::sys::statfs::statfs;
use std::io;
use std::path::Path;

#[cfg(not(target_os = "windows"))]
#[allow(clippy::cast_lossless)]
/// Returns the block size, total number of blocks, and number of blocks available for the specified directory path.
///
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
    Err(io::Error::new(
        io::ErrorKind::Other,
        "Cannot get tmp data on Windows",
    ))
}
