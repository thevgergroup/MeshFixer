//! The process's resident set size, as the operating system reports it.
//!
//! RSS counts everything the process has in RAM: the heap the counting
//! allocator sees, plus code, stacks, allocator slack, driver memory and
//! (on unified-memory machines) GPU buffers. It is reported next to the
//! counted storage figures, never instead of them (M4 Task 9).

/// Resident set size of this process in bytes. Linux reads
/// `/proc/self/status`, macOS asks `ps`, Windows asks `tasklist`. An error
/// says why there's no figure; there is never a made-up one.
pub fn process_rss_bytes() -> Result<u64, String> {
    rss()
}

#[cfg(target_os = "linux")]
fn rss() -> Result<u64, String> {
    let status = std::fs::read_to_string("/proc/self/status")
        .map_err(|e| format!("reading /proc/self/status: {e}"))?;
    let line = status
        .lines()
        .find(|l| l.starts_with("VmRSS:"))
        .ok_or("no VmRSS line in /proc/self/status")?;
    let kib: u64 = line
        .trim_start_matches("VmRSS:")
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .map_err(|e| format!("parsing {line:?}: {e}"))?;
    Ok(kib * 1024)
}

#[cfg(target_os = "macos")]
fn rss() -> Result<u64, String> {
    let pid = std::process::id().to_string();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid])
        .output()
        .map_err(|e| format!("running ps: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ps failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let kib: u64 = text
        .trim()
        .parse()
        .map_err(|e| format!("parsing ps output {text:?}: {e}"))?;
    Ok(kib * 1024)
}

#[cfg(windows)]
fn rss() -> Result<u64, String> {
    // CSV, no header: "image","pid","session","#","12,345 K"
    let filter = format!("PID eq {}", std::process::id());
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &filter, "/FO", "CSV", "/NH"])
        .output()
        .map_err(|e| format!("running tasklist: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tasklist failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let field = text
        .trim()
        .rsplit("\",\"")
        .next()
        .ok_or_else(|| format!("unexpected tasklist output {text:?}"))?;
    let digits: String = field.chars().filter(char::is_ascii_digit).collect();
    let kib: u64 = digits
        .parse()
        .map_err(|e| format!("parsing tasklist memory {field:?}: {e}"))?;
    Ok(kib * 1024)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rss() -> Result<u64, String> {
    Err("process RSS isn't implemented for this platform".into())
}
