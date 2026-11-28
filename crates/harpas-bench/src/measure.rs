//! Timing: the median of several batches, each long enough to be measured reliably

use std::hint::black_box;
use std::time::{Duration, Instant};

const BATCHES: usize = 7;
const BATCH_TIME: Duration = Duration::from_millis(60);

/// Median time of one call to `f`, in nanoseconds.
pub fn time(mut f: impl FnMut()) -> f64 {
    // Calibrate with the same batches that will be measured.
    let mut calls = 1u64;
    while batch(&mut f, calls) < BATCH_TIME {
        calls = calls.checked_mul(2).expect("benchmark batch size");
    }
    let mut samples: Vec<f64> = (0..BATCHES)
        .map(|_| batch(&mut f, calls).as_nanos() as f64 / calls as f64)
        .collect();
    samples.sort_by(f64::total_cmp);
    black_box(&samples);
    samples[BATCHES / 2]
}

fn batch(f: &mut impl FnMut(), calls: u64) -> Duration {
    let start = Instant::now();
    for _ in 0..calls {
        f();
    }
    start.elapsed()
}

/// Formats nanoseconds as microseconds with one decimal.
pub fn micros(ns: f64) -> String {
    format!("{:.1}", ns / 1000.0)
}
