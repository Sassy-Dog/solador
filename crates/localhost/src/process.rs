//! Which processes make the cut — and which table rows are processes at all.
//!
//! Mirrors `ProcessRanking.top`
//! (`Packages/HostMetricsKit/Sources/HostMetricsKit/HostSnapshot.swift`) and the
//! agent's `top_processes` (`agent/src/metrics.rs`): the **union** of the top-N
//! by CPU and the top-N by memory, deduped by pid and re-sorted by CPU
//! descending, so one list backs both a "Top CPU" and a "Top RAM" view.
//!
//! The union is the point. Top-5-by-CPU alone hides the 8 GB process sitting at
//! 0.1%, and top-5-by-memory alone hides the spinning one — so the list is
//! between N and 2N entries long, never exactly N.
//!
//! Ranking runs over *processes*, which on Linux is a narrower thing than "every
//! row sysinfo hands back" — see [`is_process`] and [`refresh_kind`], the two
//! halves of the same rule (#211).

use std::collections::HashSet;
use sysinfo::{ProcessRefreshKind, ThreadKind, UpdateKind};
use wire::Process;

/// How many to keep from each ranking. Same 5 the Swift collector and the agent
/// use, so a local card and a remote card show comparably sized lists.
pub(crate) const TOP_LIMIT: usize = 5;

/// A row of the OS process table as plain data, so the rules below are testable:
/// `sysinfo::Process`, like `sysinfo::Disk`, cannot be constructed outside that
/// crate, and this crate's other collectors (`volume::MountEntry`) already take
/// the same shape for the same reason.
pub(crate) struct ProcEntry {
    pub pid: i64,
    pub name: String,
    /// What sysinfo makes of this row: `None` for a real process (a userland
    /// thread-group leader), `Some(Userland)` for one of that leader's sibling
    /// threads, `Some(Kernel)` for a kernel thread. Always `None` off Linux,
    /// where the table has no thread rows to classify.
    pub thread_kind: Option<ThreadKind>,
    pub cpu_percent: f64,
    pub memory_mb: f64,
}

/// Whether a process-table row is a **process** — the only thing this crate's
/// `processes` list is allowed to describe (#211).
///
/// On Linux the table sysinfo hands back is really a *task* table, and both
/// kinds of non-process task would otherwise reach a host card:
///
/// - **Sibling threads.** Every thread of a multi-threaded process is its own
///   task with its own TID, and each one carries its whole process's RSS
///   (threads share one address space). One SQL Server engine therefore painted
///   five identical 1.9 GB rows in the agent's "Top RAM" and several partial
///   rows in "Top CPU" — fabricated processes, and the pid-dedupe in
///   [`top_union`] cannot collapse them because the TIDs genuinely differ.
/// - **Kernel threads.** `txg_sync` and friends are kernel tasks, not programs
///   anyone can point at; they arrive as top-level `/proc` entries no matter how
///   the table is enumerated, so [`refresh_kind`] alone would keep them.
///
/// `thread_kind()` answers both: it is `Some` for exactly those two cases and
/// `None` for a userland thread-group leader, so a single rule is the whole
/// policy.
///
/// This crate is where the bug is *latent* rather than live: the Tauri cockpit
/// ships on macOS and Windows today, where sysinfo enumerates no thread rows at
/// all and every row already reads `None`. It is the cross-platform collector,
/// though, so the day it samples a Linux box is the day the agent's #211 lands
/// here verbatim.
fn is_process(entry: &ProcEntry) -> bool {
    entry.thread_kind.is_none()
}

/// What the sampler asks sysinfo to refresh: exactly what `refresh_processes`
/// asks for, **minus the tasks**.
///
/// `refresh_processes` is documented as `…with_tasks()`, which on Linux walks
/// `/proc/<pid>/task/` for every process and files each thread it finds as
/// another process. Those rows are the fabricated entries of #211 — [`is_process`]
/// rejects them, but not asking for them is cheaper (sysinfo calls the task walk
/// "quite expensive") and means the ranking never sees a row it has to reject.
///
/// [`is_process`] still runs, and still matters: kernel threads are top-level
/// `/proc` entries that arrive with or without this flag.
///
/// **`without_tasks()` is not redundant.** `ProcessRefreshKind::nothing()` is
/// "every refresh set to `false`, *except* `tasks`" — sysinfo defaults that one
/// field to `true` on the reasoning that tasks are processes of their own on
/// Linux, which is precisely the premise #211 rejects. Building up from
/// `nothing()` therefore leaves the task walk **on** unless it is switched off by
/// name, so dropping this call would silently reduce the fix to [`is_process`]
/// alone. `the_refresh_never_asks_for_tasks` is what keeps that honest.
///
/// The rest of the set is `refresh_processes`' own, field for field, so the two
/// samplers in this repo keep asking sysinfo for the same things — the parity
/// `crates/localhost/Cargo.toml` pins the shared major version for.
pub(crate) fn refresh_kind() -> ProcessRefreshKind {
    ProcessRefreshKind::nothing()
        .with_memory()
        .with_cpu()
        .with_disk_usage()
        .with_exe(UpdateKind::OnlyIfNotSet)
        .without_tasks()
}

/// Reduces a full process table to the union described in the module docs.
///
/// Thread and kernel-thread rows are dropped **first** — see [`is_process`] — so
/// they cannot crowd a real process out of either ranking.
///
/// **A surviving row's CPU is already the whole process's.** sysinfo derives
/// per-process CPU from the `utime`/`stime` it reads out of that entry's own
/// `stat` file, and for a leader that file is `/proc/<pid>/stat`, whose times the
/// kernel reports thread-group-wide (`do_task_stat` with `whole`) — per-thread
/// times live in the separate `/proc/<pid>/task/<tid>/stat`. So dropping the
/// sibling rows loses no CPU time, and summing them back in would double-count
/// it. Memory is the same story from the other side: the leader's RSS *is* the
/// process's RSS, which is why it appeared N times to begin with.
pub(crate) fn top_union(entries: Vec<ProcEntry>, limit: usize) -> Vec<Process> {
    let all: Vec<Process> = entries
        .into_iter()
        .filter(is_process)
        .map(|entry| Process {
            pid: entry.pid,
            name: entry.name,
            cpu_percent: entry.cpu_percent,
            memory_mb: entry.memory_mb,
        })
        .collect();

    let mut by_cpu = all.clone();
    by_cpu.sort_by(descending(|p| p.cpu_percent));
    let mut by_memory = all;
    by_memory.sort_by(descending(|p| p.memory_mb));

    let mut seen: HashSet<i64> = HashSet::new();
    let mut out: Vec<Process> = Vec::new();
    for process in by_cpu.into_iter().take(limit) {
        if seen.insert(process.pid) {
            out.push(process);
        }
    }
    for process in by_memory.into_iter().take(limit) {
        if seen.insert(process.pid) {
            out.push(process);
        }
    }
    out.sort_by(descending(|p| p.cpu_percent));
    out
}

/// Descending comparator over an `f64` field. A NaN compares equal rather than
/// panicking: `sort_by` with an inconsistent comparator is a documented way to
/// get a panic out of the standard library, and one unreadable process must not
/// take the whole list down.
fn descending(key: fn(&Process) -> f64) -> impl Fn(&Process, &Process) -> std::cmp::Ordering {
    move |a, b| {
        key(b)
            .partial_cmp(&key(a))
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real process: a userland thread-group leader, the only kind of row that
    /// is allowed through.
    fn process(pid: i64, cpu_percent: f64, memory_mb: f64) -> ProcEntry {
        entry(pid, &format!("proc{pid}"), None, cpu_percent, memory_mb)
    }

    fn entry(
        pid: i64,
        name: &str,
        thread_kind: Option<ThreadKind>,
        cpu_percent: f64,
        memory_mb: f64,
    ) -> ProcEntry {
        ProcEntry {
            pid,
            name: name.to_string(),
            thread_kind,
            cpu_percent,
            memory_mb,
        }
    }

    fn pids(processes: &[Process]) -> Vec<i64> {
        processes.iter().map(|p| p.pid).collect()
    }

    /// The union, not the intersection and not either list alone: the RAM hog at
    /// 0% CPU has to survive, or the "Top RAM" half of the card is a lie.
    #[test]
    fn a_memory_hog_at_zero_cpu_still_makes_the_list() {
        let all = vec![
            process(1, 90.0, 10.0),
            process(2, 80.0, 10.0),
            process(3, 0.0, 8000.0),
        ];

        let top = top_union(all, 2);

        assert_eq!(pids(&top), vec![1, 2, 3]);
    }

    #[test]
    fn a_process_topping_both_rankings_appears_once() {
        let all = vec![
            process(1, 90.0, 9000.0),
            process(2, 10.0, 100.0),
            process(3, 5.0, 50.0),
        ];

        let top = top_union(all, 1);

        assert_eq!(pids(&top), vec![1]);
    }

    /// The memory picks are appended in *memory* order, so the final re-sort is
    /// what puts the whole list back into CPU order — here pulling pid 4 ahead
    /// of pid 3, which the append order had the other way round.
    #[test]
    fn the_result_is_sorted_by_cpu_descending_even_though_memory_picks_ran_last() {
        let all = vec![
            process(1, 90.0, 1.0),
            process(2, 80.0, 2.0),
            process(3, 5.0, 9000.0),
            process(4, 20.0, 8000.0),
        ];

        let top = top_union(all, 2);

        assert_eq!(pids(&top), vec![1, 2, 4, 3]);
    }

    /// Between `limit` and `2 * limit`, never exactly `limit` when the two
    /// rankings disagree — the shape a consumer sizing the list must expect.
    #[test]
    fn the_union_is_at_most_twice_the_limit() {
        let all: Vec<ProcEntry> = (0..50)
            .map(|i| process(i64::from(i), f64::from(i), f64::from(50 - i)))
            .collect();

        let top = top_union(all, TOP_LIMIT);

        assert_eq!(top.len(), TOP_LIMIT * 2);
    }

    #[test]
    fn fewer_processes_than_the_limit_all_come_back() {
        let all = vec![process(1, 1.0, 1.0), process(2, 2.0, 2.0)];

        let top = top_union(all, TOP_LIMIT);

        assert_eq!(pids(&top), vec![2, 1]);
    }

    #[test]
    fn an_empty_list_stays_empty() {
        assert!(top_union(Vec::new(), TOP_LIMIT).is_empty());
    }

    // -----------------------------------------------------------------------
    // Threads are not processes (#211).
    //
    // These fixtures stand in for a Linux process table on purpose: the rows
    // they describe are the ones sysinfo only ever produces there, and the
    // cockpit runs on macOS/Windows today. Pinning the rule by fixture is what
    // makes it a rule rather than a platform accident.
    // -----------------------------------------------------------------------

    /// The ubu-3xdv process table, as sysinfo hands it over on Linux: ONE SQL
    /// Server engine (pid 8723) whose sibling threads are each their own row
    /// carrying the whole process's 1.9 GB RSS and a slice of its CPU, the
    /// watchdog process beside it (pid 5371), and a ZFS kernel thread.
    fn sqlservr_table() -> Vec<ProcEntry> {
        vec![
            // The engine: a thread-group leader, so its CPU is already the
            // group's total and its RSS the group's RSS.
            entry(8723, "sqlservr", None, 341.0, 1900.0),
            // Its threads. Distinct TIDs, so the pid-dedupe never touched them.
            entry(8724, "sqlservr", Some(ThreadKind::Userland), 201.0, 1900.0),
            entry(8725, "sqlservr", Some(ThreadKind::Userland), 90.0, 1900.0),
            entry(8726, "sqlservr", Some(ThreadKind::Userland), 26.0, 1900.0),
            entry(8727, "sqlservr", Some(ThreadKind::Userland), 24.0, 1900.0),
            // The watchdog: a second, genuinely separate process.
            entry(5371, "sqlservr", None, 0.1, 3.6),
            // A ZFS kernel thread — not a program, never a row on a host card.
            // Busy enough to place near the top by CPU, which is exactly how it
            // came to be painted into "Top CPU" on the real host.
            entry(412, "txg_sync", Some(ThreadKind::Kernel), 95.0, 0.0),
        ]
    }

    /// THE #211 BUG: a multi-threaded process is ONE row, not one per thread.
    /// "Top RAM" showed five 1.9 GB `sqlservr` rows for a single engine because
    /// every thread reports its process's RSS, and the pid-dedupe cannot see it
    /// — the TIDs are genuinely different pids.
    #[test]
    fn a_multi_threaded_process_yields_exactly_one_row() {
        let top = top_union(sqlservr_table(), TOP_LIMIT);

        let engine = top.iter().filter(|p| p.pid == 8723).count();
        assert_eq!(engine, 1, "the engine must appear once, got {top:?}");

        let threads: Vec<i64> = pids(&top)
            .into_iter()
            .filter(|pid| (8724..=8727).contains(pid))
            .collect();
        assert!(
            threads.is_empty(),
            "thread TIDs must not reach a host card, got {threads:?}"
        );

        // The one row that is not the engine is the watchdog — a real second
        // process, which the filter must NOT swallow along with the threads.
        let mut survivors = pids(&top);
        survivors.sort_unstable();
        assert_eq!(survivors, vec![5371, 8723]);
    }

    /// The surviving row carries the process-level figures: the CPU sysinfo
    /// already aggregated across the thread group (never the sum of the thread
    /// rows on top of it — that would double-count), and the RSS exactly once
    /// rather than once per thread.
    #[test]
    fn the_surviving_row_carries_process_level_cpu_and_a_single_rss() {
        let top = top_union(sqlservr_table(), TOP_LIMIT);

        let engine = top.iter().find(|p| p.pid == 8723).expect("engine survives");
        assert!(
            (engine.cpu_percent - 341.0).abs() < f64::EPSILON,
            "CPU must be the leader's group-wide figure, got {}",
            engine.cpu_percent
        );

        let rss_rows = top.iter().filter(|p| p.memory_mb == 1900.0).count();
        assert_eq!(rss_rows, 1, "1.9 GB is one process's RSS, counted once");
    }

    /// Kernel threads (`txg_sync` and the rest of the `[bracketed]` crowd) are
    /// tasks, not programs. They arrive as top-level `/proc` entries whatever
    /// the refresh asks for, so the filter — not [`refresh_kind`] — is what
    /// keeps them off a host card.
    ///
    /// The kernel thread here out-ranks the process on BOTH axes, so it is the
    /// first row an unfiltered ranking would emit: the assertion cannot pass by
    /// the kernel thread quietly missing the cut.
    #[test]
    fn kernel_threads_never_reach_the_process_list() {
        let table = vec![
            entry(412, "txg_sync", Some(ThreadKind::Kernel), 300.0, 4096.0),
            entry(8723, "sqlservr", None, 12.0, 1900.0),
        ];

        let top = top_union(table, TOP_LIMIT);

        assert!(
            !top.iter().any(|p| p.name == "txg_sync" || p.pid == 412),
            "a kernel thread is not a process, got {top:?}"
        );
        // …and the real process below it still comes through.
        assert_eq!(pids(&top), vec![8723]);
    }

    /// Filtering happens BEFORE the top-N cut, so threads cannot crowd real
    /// processes out of the list: ten thread rows ahead of it must not cost the
    /// quiet 8 GB process its place.
    #[test]
    fn threads_do_not_consume_top_n_slots() {
        let mut table = vec![process(999, 0.2, 8192.0)];
        for tid in 8724..8734 {
            table.push(entry(
                tid,
                "sqlservr",
                Some(ThreadKind::Userland),
                200.0,
                1900.0,
            ));
        }
        table.push(entry(8723, "sqlservr", None, 341.0, 1900.0));

        let top = top_union(table, TOP_LIMIT);

        let mut survivors = pids(&top);
        survivors.sort_unstable();
        assert_eq!(survivors, vec![999, 8723]);
    }

    /// The refresh this crate asks for must not populate the thread rows in the
    /// first place — that is the cheap half of the fix, and the half a reader
    /// cannot see from [`is_process`] alone. Everything else stays exactly what
    /// `refresh_processes` asks for, since those are the fields the mapping in
    /// `lib.rs` reads back.
    #[test]
    fn the_refresh_never_asks_for_tasks() {
        let kind = refresh_kind();

        assert!(!kind.tasks(), "task rows are what #211 is made of");
        assert!(kind.cpu());
        assert!(kind.memory());
        assert_eq!(kind.exe(), UpdateKind::OnlyIfNotSet);
    }

    /// A NaN reading must not panic the sort. `sort_by` is allowed to panic when
    /// the comparator is inconsistent, and an unreadable process is not a reason
    /// to lose the whole list.
    #[test]
    fn a_nan_reading_does_not_panic_the_sort() {
        let all = vec![
            process(1, f64::NAN, 10.0),
            process(2, 5.0, f64::NAN),
            process(3, 1.0, 1.0),
        ];

        let top = top_union(all, TOP_LIMIT);

        assert_eq!(top.len(), 3);
    }
}
