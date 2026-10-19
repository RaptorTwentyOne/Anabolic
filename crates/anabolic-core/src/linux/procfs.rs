//! Reading /proc: process and thread status lines, and the system facts
//! (clock ticks, boot time) needed to turn them into the model's units.

use std::sync::OnceLock;

/// `PF_KTHREAD` in a process' flags: a kernel thread.
pub const PF_KTHREAD: u32 = 0x0020_0000;

/// What /proc/<pid>/stat (or a task's) says.
#[derive(Clone, Debug, Default)]
pub struct Stat {
    pub pid: u32,
    pub comm: String,
    /// R, S, D, T, t, Z, X, I, P…
    pub state: u8,
    pub ppid: u32,
    pub flags: u32,
    pub minflt: u64,
    pub majflt: u64,
    /// Clock ticks.
    pub utime: u64,
    pub stime: u64,
    /// The kernel's priority: 20 + nice for normal threads, negative for
    /// real-time ones.
    pub priority: i64,
    pub nice: i64,
    pub num_threads: u32,
    /// Clock ticks since boot.
    pub starttime: u64,
    pub vsize: u64,
    pub rss_pages: u64,
    pub policy: u32,
}

impl Stat {
    pub fn kernel_thread(&self) -> bool {
        self.flags & PF_KTHREAD != 0
    }

    /// When it started, as a FILETIME: stable for the life of the process,
    /// so with the PID it names one process instance.
    pub fn create_time(&self) -> i64 {
        start_filetime(self.starttime)
    }

    /// Kernel and user time in 100 ns units.
    pub fn kernel_time(&self) -> u64 {
        ticks_to_100ns(self.stime)
    }

    pub fn user_time(&self) -> u64 {
        ticks_to_100ns(self.utime)
    }
}

/// Parses a stat line. The name sits in parentheses and may itself contain
/// spaces and parentheses, so the fields are counted from the last ')'.
pub fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text[..open].trim().parse().ok()?;
    let comm = text.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = text.get(close + 2..)?.split_ascii_whitespace().collect();
    // rest[0] is field 3 (state).
    let field = |n: usize| rest.get(n - 3).copied();
    let num = |n: usize| field(n).and_then(|f| f.parse::<u64>().ok()).unwrap_or(0);
    let signed = |n: usize| field(n).and_then(|f| f.parse::<i64>().ok()).unwrap_or(0);
    Some(Stat {
        pid,
        comm,
        state: field(3)?.bytes().next()?,
        ppid: num(4) as u32,
        flags: num(9) as u32,
        minflt: num(10),
        majflt: num(12),
        utime: num(14),
        stime: num(15),
        priority: signed(18),
        nice: signed(19),
        num_threads: num(20) as u32,
        starttime: num(22),
        vsize: num(23),
        rss_pages: num(24),
        policy: num(41) as u32,
    })
}

pub fn stat(pid: u32) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

pub fn task_stat(pid: u32, tid: u32) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat")).ok()?)
}

/// The value of one `Key:\tvalue` line of a status file.
pub fn status_value<'a>(status: &'a str, key: &str) -> Option<&'a str> {
    status.lines().find_map(|line| {
        let rest = line.strip_prefix(key)?.strip_prefix(':')?;
        Some(rest.trim())
    })
}

/// A `Key:  1234 kB` status value in bytes.
pub fn status_kb(status: &str, key: &str) -> Option<u64> {
    let value = status_value(status, key)?;
    let n: u64 = value.split_ascii_whitespace().next()?.parse().ok()?;
    Some(n * 1024)
}

/// Every PID in /proc, in no particular order.
pub fn pids() -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

/// The thread ids of a process.
pub fn tids(pid: u32) -> Vec<u32> {
    let Ok(dir) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

/// The command line, its arguments separated by spaces; empty for kernel
/// threads and processes that are ending.
pub fn cmdline(pid: u32) -> Option<String> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
    Some(
        raw.split(|&b| b == 0)
            .map(|a| String::from_utf8_lossy(a))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// The arguments as they were given, unjoined.
pub fn argv(pid: u32) -> Vec<String> {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return Vec::new();
    };
    let raw = raw.strip_suffix(&[0]).unwrap_or(&raw);
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(|&b| b == 0)
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect()
}

/// The real user id of a process (from its status), when readable.
pub fn uid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status_value(&status, "Uid")?
        .split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

/// How many clock ticks make a second (USER_HZ, nearly always 100).
pub fn clock_ticks() -> u64 {
    static TICKS: OnceLock<u64> = OnceLock::new();
    *TICKS.get_or_init(|| {
        let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if t > 0 { t as u64 } else { 100 }
    })
}

pub fn page_size() -> u64 {
    static PAGE: OnceLock<u64> = OnceLock::new();
    *PAGE.get_or_init(|| {
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as u64 } else { 4096 }
    })
}

pub fn ticks_to_100ns(ticks: u64) -> u64 {
    ticks * (10_000_000 / clock_ticks())
}

/// When the machine started, as a FILETIME. The kernel gives whole seconds
/// (btime), the same every time it is read, so start times built on it
/// stay stable.
pub fn boot_filetime() -> i64 {
    static BOOT: OnceLock<i64> = OnceLock::new();
    *BOOT.get_or_init(|| {
        let secs = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("btime ")?.trim().parse::<i64>().ok())
            })
            .unwrap_or(0);
        crate::util::filetime_from_unix(secs as f64)
    })
}

/// A start time in clock ticks since boot, as a FILETIME.
pub fn start_filetime(starttime: u64) -> i64 {
    boot_filetime() + ticks_to_100ns(starttime) as i64
}

/// The executable's path, which needs the right to look into the process.
pub fn exe(pid: u32) -> Option<String> {
    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let path = path.to_string_lossy();
    // A replaced or deleted file keeps its old name, marked.
    Some(path.trim_end_matches(" (deleted)").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_name_with_spaces_and_parentheses() {
        let line = "1234 (my (odd) name) S 1 1234 1234 0 -1 4194560 10 0 2 0 7 3 0 0 20 0 4 0 \
                    5000 1000000 300 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 5 0 0 0 0 0";
        let s = parse_stat(line).unwrap();
        assert_eq!(s.pid, 1234);
        assert_eq!(s.comm, "my (odd) name");
        assert_eq!(s.state, b'S');
        assert_eq!(s.ppid, 1);
        assert_eq!((s.minflt, s.majflt), (10, 2));
        assert_eq!((s.utime, s.stime), (7, 3));
        assert_eq!((s.priority, s.nice), (20, 0));
        assert_eq!(s.num_threads, 4);
        assert_eq!(s.starttime, 5000);
        assert_eq!(s.vsize, 1_000_000);
        assert_eq!(s.rss_pages, 300);
    }

    #[test]
    fn reads_itself() {
        let s = stat(std::process::id()).unwrap();
        assert_eq!(s.pid, std::process::id());
        assert!(s.num_threads >= 1);
        assert!(!s.kernel_thread());
        let now = crate::util::filetime_from_unix(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs_f64(),
        );
        // Started in the last hour, give or take btime's rounding.
        assert!(s.create_time() <= now + 20_000_000);
        assert!(s.create_time() > now - 36_000_000_000);
        assert!(tids(std::process::id()).contains(&std::process::id()));
        assert!(uid(std::process::id()).is_some());
    }
}
