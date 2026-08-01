//! Which processes make the cut.
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

use std::collections::HashSet;
use wire::Process;

/// How many to keep from each ranking. Same 5 the Swift collector and the agent
/// use, so a local card and a remote card show comparably sized lists.
pub(crate) const TOP_LIMIT: usize = 5;

/// Reduces a full process list to the union described in the module docs.
pub(crate) fn top_union(all: Vec<Process>, limit: usize) -> Vec<Process> {
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

    fn process(pid: i64, cpu_percent: f64, memory_mb: f64) -> Process {
        Process {
            pid,
            name: format!("proc{pid}"),
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
        let all: Vec<Process> = (0..50)
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
