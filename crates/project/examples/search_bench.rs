//! Times file indexing and project search on a real repository.
//!
//! `cargo run -q --release --no-default-features -p ion_project --example search_bench -- <root> [runs]`
//!
//! Numbers are warm-cache (the first run of each step is discarded). Pair with
//! `/usr/bin/time -l` (macOS) or `Measure-Command` (Windows) for peak memory.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use ion_project::FileIndex;
use ion_project::search::{Sources, search_streaming};

/// (query, case sensitive). Ion searches case-insensitively by default.
const QUERIES: &[(&str, bool)] = &[
    // Hits the result cap: stops early.
    ("EXPORT_SYMBOL_GPL", false),
    // Hundreds of matches.
    ("kmalloc_array", false),
    // A few dozen matches.
    ("pci_disable_sriov", false),
    // No matches: a full scan.
    ("zzqx_no_such_identifier", false),
    ("zzqx_no_such_identifier", true),
    ("kmalloc_array", true),
];

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort();
    times[times.len() / 2]
}

fn time<T>(runs: usize, mut f: impl FnMut() -> T) -> (Duration, Duration, T) {
    let mut result = f();
    let mut times = Vec::with_capacity(runs);
    for _ in 0..runs {
        let start = Instant::now();
        result = f();
        times.push(start.elapsed());
    }
    let min = *times.iter().min().expect("at least one run");
    (median(times), min, result)
}

fn ms(duration: Duration) -> String {
    format!("{:.0} ms", duration.as_secs_f64() * 1000.)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("usage: search_bench <root> [runs]"));
    let runs: usize = args.next().map_or(5, |runs| runs.parse().expect("runs"));
    let mut out = std::io::stdout().lock();

    let (median_time, min_time, index) = time(runs, || FileIndex::build(&root));
    let (bytes, searchable) = index
        .files
        .iter()
        .filter_map(|file| std::fs::metadata(index.absolute(file)).ok())
        .filter(|meta| meta.len() <= 4 * 1024 * 1024)
        .fold((0u64, 0usize), |(bytes, n), meta| {
            (bytes + meta.len(), n + 1)
        });
    writeln!(
        out,
        "{} files indexed ({searchable} up to 4 MiB, {:.2} GB), {runs} runs, {} threads",
        index.files.len(),
        bytes as f64 / 1e9,
        std::thread::available_parallelism().map_or(0, |n| n.get())
    )
    .ok();
    writeln!(
        out,
        "{:<34} {:>9} {:>9} {:>8} {:>7} {:>9}",
        "step", "median", "min", "lines", "files", "first"
    )
    .ok();
    writeln!(
        out,
        "{:<34} {:>9} {:>9}",
        "index build",
        ms(median_time),
        ms(min_time)
    )
    .ok();

    for &(query, case_sensitive) in QUERIES {
        let cancel = AtomicBool::new(false);
        let mut firsts = Vec::new();
        let (median_time, min_time, (lines, files, truncated)) = time(runs, || {
            let start = Instant::now();
            let first = Mutex::new(None);
            let found = Mutex::new((0, 0));
            let truncated = search_streaming(
                &index,
                query,
                case_sensitive,
                &Sources::default(),
                &cancel,
                &|batch| {
                    first.lock().unwrap().get_or_insert_with(|| start.elapsed());
                    let mut found = found.lock().unwrap();
                    found.0 += batch.iter().map(|file| file.lines.len()).sum::<usize>();
                    found.1 += batch.len();
                },
            );
            if let Some(first) = first.into_inner().unwrap() {
                firsts.push(first);
            }
            let (lines, files) = found.into_inner().unwrap();
            (lines, files, truncated)
        });
        let label = format!(
            "search {query}{}",
            if case_sensitive { " (Aa)" } else { "" }
        );
        writeln!(
            out,
            "{label:<34} {:>9} {:>9} {:>7}{} {:>7} {:>9}",
            ms(median_time),
            ms(min_time),
            lines,
            if truncated { "+" } else { " " },
            files,
            if firsts.is_empty() {
                "-".into()
            } else {
                ms(median(firsts))
            }
        )
        .ok();
    }
}
