// SPDX-License-Identifier: GPL-3.0-or-later

//! Manual WR-015 measurement harness. Budgets stay in the evidence report,
//! never in assertions, so normal test runs remain deterministic.

use std::path::PathBuf;
use std::time::Instant;

use warcraft_recorder::storage::Storage;

#[test]
#[ignore = "run manually with WR015_CORPUS after generating the WR-000 corpus"]
fn measure_library_scan() {
    let root = PathBuf::from(std::env::var_os("WR015_CORPUS").expect("set WR015_CORPUS"));
    let capture = root.join(".wr015-capture");
    let storage = Storage::new(root, capture);
    let mut samples = Vec::new();

    for run in 0..6 {
        let started = Instant::now();
        let index = storage.scan();
        let elapsed = started.elapsed();
        assert_eq!(index.entries.len(), 2_000);
        assert_eq!(index.correlations.len(), 1_900);
        assert!(index.skipped.is_empty());
        if run > 0 {
            samples.push(elapsed.as_micros());
        }
    }
    samples.sort_unstable();
    println!("scan_us={samples:?} median_us={}", samples[2]);
}
