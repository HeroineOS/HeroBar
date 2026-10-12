//! The programs using the most memory, for the memory module's popup. A
//! program's processes count as one: a process whose parent runs the same
//! executable (Firefox's and Chromium's tabs) joins the parent's.

use std::collections::HashMap;

/// (name, KiB), biggest first.
pub fn top(n: usize) -> Vec<(String, u64)> {
    struct P {
        ppid: u32,
        exe: String,
        comm: String,
        kib: u64,
    }
    // Pages not shared with other processes (resident - shared).
    let page_kib = 4;
    let mut all: HashMap<u32, P> = HashMap::new();
    let Ok(rd) = std::fs::read_dir("/proc") else { return vec![] };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        let dir = e.path();
        let Ok(stat) = std::fs::read_to_string(dir.join("stat")) else { continue };
        // "pid (comm) S ppid ..." - comm may hold spaces and parentheses.
        let Some(close) = stat.rfind(')') else { continue };
        let ppid = stat[close + 1..].split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let Ok(statm) = std::fs::read_to_string(dir.join("statm")) else { continue };
        let f: Vec<u64> = statm.split_whitespace().filter_map(|s| s.parse().ok()).collect();
        let kib = f.get(1).copied().unwrap_or(0).saturating_sub(f.get(2).copied().unwrap_or(0)) * page_kib;
        if kib == 0 {
            continue; // kernel threads
        }
        let exe = std::fs::read_link(dir.join("exe")).ok().and_then(|p| p.to_str().map(|s| s.trim_end_matches(" (deleted)").to_owned())).unwrap_or_default();
        let comm = std::fs::read_to_string(dir.join("comm")).unwrap_or_default().trim().to_owned();
        all.insert(pid, P { ppid, exe, comm, kib });
    }
    let mut sums: HashMap<u32, u64> = HashMap::new();
    for (&pid, p) in &all {
        let mut root = pid;
        let mut cur = p;
        while let Some(parent) = all.get(&cur.ppid) {
            if cur.exe.is_empty() || parent.exe != cur.exe {
                break;
            }
            root = cur.ppid;
            cur = parent;
        }
        *sums.entry(root).or_default() += p.kib;
    }
    let mut by_name: HashMap<String, u64> = HashMap::new();
    for (root, kib) in sums {
        let p = &all[&root];
        let base = p.exe.rsplit('/').next().unwrap_or("");
        let name = crate::apps::by_id(base).map(|a| a.name).filter(|n| !n.is_empty()).unwrap_or_else(|| if p.comm.is_empty() { base.to_owned() } else { p.comm.clone() });
        *by_name.entry(name).or_default() += kib;
    }
    let mut v: Vec<(String, u64)> = by_name.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

pub fn size(kib: u64) -> String {
    if kib >= 1024 * 1024 {
        format!("{:.1} GiB", kib as f64 / 1024.0 / 1024.0)
    } else {
        format!("{} MiB", kib / 1024)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_something() {
        let v = super::top(5);
        assert!(!v.is_empty());
        assert!(v.windows(2).all(|w| w[0].1 >= w[1].1));
    }
}
