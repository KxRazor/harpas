//! A region file read from disk, as a downloaded world provides it. Every chunk is read, and a region that
//! opens is saved and must read back with the same chunks. `seeds/region` holds small valid regions to
//! start from: the fuzzer does not find the 8 KiB header on its own.
#![no_main]

use std::fs;
use std::path::PathBuf;
use std::sync::LazyLock;

use harpas_anvil::Region;
use libfuzzer_sys::fuzz_target;

static DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join(format!("harpas-fuzz-region-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("the temporary folder can be created");
    dir
});

fuzz_target!(|data: &[u8]| {
    for entry in fs::read_dir(&*DIR).expect("the temporary folder can be read") {
        let _ = fs::remove_file(entry.expect("its entries can be read").path());
    }
    fs::write(DIR.join("r.0.0.mca"), data).expect("the region file can be written");
    let Ok(mut region) = Region::open(&DIR, 0, 0) else {
        return;
    };
    let chunks: Vec<_> = (0..32 * 32)
        .map(|index| region.chunk(index % 32, index / 32).ok())
        .collect();
    // A region with an unreadable entry refuses to save rather than lose it.
    if chunks.iter().all(Option::is_some) && region.save().is_ok() {
        let saved = Region::open(&DIR, 0, 0).expect("a saved region opens");
        for (index, chunk) in (0..32 * 32).zip(&chunks) {
            assert_eq!(saved.chunk(index % 32, index / 32).ok(), *chunk);
        }
    }
});
