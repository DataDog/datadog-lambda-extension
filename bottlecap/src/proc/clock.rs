use libc;
use std::io;

#[allow(clippy::cast_sign_loss)]
#[cfg(not(target_os = "windows"))]
pub fn get_clk_tck() -> Result<u64, io::Error> {
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if clk_tck == -1 {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "Failed to get clock ticks per second",
        ));
    }
    Ok(clk_tck as u64)
}

#[cfg(target_os = "windows")]
pub fn get_clk_tck() -> Result<u64, io::Error> {
    // Windows does not have this concept
    Ok(1)
}
