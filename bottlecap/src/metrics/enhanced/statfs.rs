#![allow(clippy::module_name_repetitions)]

use crate::metrics::enhanced::constants;
use nix::sys::statfs::statfs;
use std::io;
use std::path::Path;

#[cfg(not(target_os = "windows"))]
#[allow(clippy::cast_lossless)]
/// Returns the block size, total number of blocks, and number of blocks available for the specified directory path.
///
pub fn statfs_info(path: &str) -> Result<(f64, f64, f64), io::Error> {
    let stat = statfs(Path::new(path)).map_err(io::Error::other)?;
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

pub fn get_tmp_max() -> Result<f64, io::Error> {
    let (bsize, blocks, _) = statfs_info(constants::TMP_PATH)?;
    let tmp_max = bsize * blocks;
    Ok(tmp_max)
}

pub fn get_tmp_used() -> Result<f64, io::Error> {
    let (bsize, blocks, bavail) = statfs_info(constants::TMP_PATH)?;
    let tmp_used = bsize * (blocks - bavail);
    Ok(tmp_used)
}
