//! Timing: the median of several batches, each long enough to be measured reliably

use std::hint::black_box;
use std::time::{Duration, Instant};

const BATCHES: usize = 7;
const BATCH_TIME: Duration = Duration::from_millis(60);

/// Median time of one call to `f`, in nanoseconds.
pub fn time(mut f: impl FnMut()) -> f64 {
    // Warm up, and find how many calls fill a batch.
    let start = Instant::now();
    let mut calls = 0u64;
    while start.elapsed() < BATCH_TIME {
        f();
        calls += 1;
    }
    let mut samples: Vec<f64> = (0..BATCHES)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..calls {
                f();
            }
            start.elapsed().as_nanos() as f64 / calls as f64
        })
        .collect();
    samples.sort_by(f64::total_cmp);
    black_box(&samples);
    samples[BATCHES / 2]
}

/// Formats nanoseconds as microseconds with one decimal.
pub fn micros(ns: f64) -> String {
    format!("{:.1}", ns / 1000.0)
}
