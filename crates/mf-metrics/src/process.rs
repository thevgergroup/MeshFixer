//! The process's memory, as the operating system reports it.
//!
//! - **Resident** (RSS): pages in RAM right now. It excludes pages the OS
//!   has compressed or swapped out, so under memory pressure it can be far
//!   below what the process holds (M4 Task 10 fix round 1, I1).
//! - **Physical footprint** (macOS): what Activity Monitor shows as
//!   "Memory": resident, compressed and swapped pages the process owns,
//!   GPU buffers in shared memory included. The figure to read on a Mac.
//! - **Page-ins**: pages read back from disk so far (the process waited on
//!   swap or a mapped file); the frame gates use them to tell a swapping
//!   machine from a slow app.
//!
//! Each is reported next to the counted storage figures, never instead of
//! them. A figure the OS doesn't give is an error or `None`, never a guess.

/// What the OS says about this process's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessMemory {
    pub resident_bytes: u64,
    /// macOS only.
    pub phys_footprint_bytes: Option<u64>,
    /// Highest physical footprint since the process started (macOS only).
    pub peak_phys_footprint_bytes: Option<u64>,
    /// Pages read in from disk since the start (macOS `ri_pageins`, Linux
    /// major faults); `None` where unknown.
    pub page_ins: Option<u64>,
}

/// This process's memory now. No process is spawned on Linux or macOS
/// (a syscall or `/proc`), so it is cheap enough for a UI thread there;
/// Windows asks `tasklist`.
pub fn process_memory() -> Result<ProcessMemory, String> {
    memory()
}

/// Resident set size in bytes (see [`ProcessMemory::resident_bytes`]).
pub fn process_rss_bytes() -> Result<u64, String> {
    memory().map(|m| m.resident_bytes)
}

/// Page-ins so far (see [`ProcessMemory::page_ins`]).
pub fn page_ins() -> Result<u64, String> {
    memory()?
        .page_ins
        .ok_or_else(|| "page-ins aren't reported on this platform".into())
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // one libc call, with a buffer of the type it fills
fn memory() -> Result<ProcessMemory, String> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: `info` is a writable rusage_info_v4, the struct the
    // RUSAGE_INFO_V4 flavour fills; the pointer is only used for the call.
    let rc = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            info.as_mut_ptr().cast::<libc::rusage_info_t>(),
        )
    };
    if rc != 0 {
        return Err(format!(
            "proc_pid_rusage failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the call succeeded, so the kernel filled the whole struct
    // (and it was zeroed before, so every byte is initialised anyway).
    let info = unsafe { info.assume_init() };
    Ok(ProcessMemory {
        resident_bytes: info.ri_resident_size,
        phys_footprint_bytes: Some(info.ri_phys_footprint),
        peak_phys_footprint_bytes: Some(info.ri_lifetime_max_phys_footprint),
        page_ins: Some(info.ri_pageins),
    })
}

#[cfg(target_os = "linux")]
fn memory() -> Result<ProcessMemory, String> {
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
    // /proc/self/stat: majflt is field 12; fields after the ")" closing
    // the command name start at field 3.
    let stat = std::fs::read_to_string("/proc/self/stat")
        .map_err(|e| format!("reading /proc/self/stat: {e}"))?;
    let majflt = stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(12 - 3))
        .ok_or_else(|| format!("no majflt field in /proc/self/stat: {stat:?}"))?
        .parse::<u64>()
        .map_err(|e| format!("parsing majflt in /proc/self/stat: {e}"))?;
    Ok(ProcessMemory {
        resident_bytes: kib * 1024,
        phys_footprint_bytes: None,
        peak_phys_footprint_bytes: None,
        page_ins: Some(majflt),
    })
}

#[cfg(windows)]
fn memory() -> Result<ProcessMemory, String> {
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
    Ok(ProcessMemory {
        resident_bytes: kib * 1024,
        phys_footprint_bytes: None,
        peak_phys_footprint_bytes: None,
        page_ins: None,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn memory() -> Result<ProcessMemory, String> {
    Err("process memory isn't implemented for this platform".into())
}
