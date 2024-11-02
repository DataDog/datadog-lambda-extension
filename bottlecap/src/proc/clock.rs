use nix::unistd::sysconf;
use nix::unistd::SysconfVar;
use std::io;

#[allow(clippy::cast_sign_loss)]
#[cfg(not(target_os = "windows"))]
pub fn get_clk_tck() -> Result<u64, io::Error> {
    match sysconf(SysconfVar::CLK_TCK) {
        Ok(Some(clk_tck)) if clk_tck > 0 => Ok(clk_tck as u64),
        _ => Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Could not find system clock ticks per second",
        )),
    }
}

#[cfg(target_os = "windows")]
pub fn get_clk_tck() -> Result<u64, io::Error> {
    // Windows does not have this concept
    Ok(1)
}
