//! Measures how well each part of a saved index compresses under different settings.
//! Usage: cargo run --release -p bs-index --example snapshot_sizes -- <index.bin>

use std::time::Instant;

use bs_index::{Index, NO_PARENT};

fn zsize(data: &[u8], level: i32, long: bool) -> (usize, f64) {
    let t = Instant::now();
    let mut enc = zstd::bulk::Compressor::new(level).unwrap();
    if long {
        enc.long_distance_matching(true).unwrap();
        enc.window_log(27).unwrap();
    }
    let out = enc.compress(data).unwrap();
    (out.len(), t.elapsed().as_secs_f64() * 1000.0)
}

fn u32s(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// Stores each value as the difference to the previous one, so runs of nearby
/// numbers become runs of small numbers.
fn delta(v: &[u32]) -> Vec<u32> {
    let mut prev = 0u32;
    v.iter()
        .map(|&x| {
            let d = x.wrapping_sub(prev);
            prev = x;
            d
        })
        .collect()
}

fn main() {
    let path = std::env::args().nth(1).expect("path to index.bin");
    let index = Index::load(path.as_ref()).unwrap();
    let n = index.len() as u32;
    let parents: Vec<u32> = (0..n)
        .map(|e| index.parent(e).unwrap_or(NO_PARENT))
        .collect();
    let parent_gap: Vec<u32> = parents
        .iter()
        .enumerate()
        .map(|(e, &p)| (e as u32).wrapping_sub(p))
        .collect();
    let mut lookups = Vec::new();
    for v in index.volumes() {
        let mut r = 0u64;
        let mut misses = 0;
        while misses < 1_000_000 {
            match v.entry_for_record(r) {
                Some(e) => {
                    lookups.push(e);
                    misses = 0;
                }
                None => {
                    lookups.push(NO_PARENT);
                    misses += 1;
                }
            }
            r += 1;
        }
        lookups.truncate(lookups.len() - misses);
    }
    let parts: Vec<(&str, Vec<u8>)> = vec![
        ("names text", index.names().folded_buffer().to_vec()),
        ("name offsets", u32s(index.names().offsets())),
        ("name offsets delta", u32s(&delta(index.names().offsets()))),
        ("entry name ids", u32s(index.name_ids())),
        ("entry name ids delta", u32s(&delta(index.name_ids()))),
        ("parents", u32s(&parents)),
        ("parents delta", u32s(&delta(&parents))),
        ("parents as gap", u32s(&parent_gap)),
        ("flags", index.flags().to_vec()),
        ("record lookup", u32s(&lookups)),
        ("record lookup delta", u32s(&delta(&lookups))),
    ];
    println!(
        "{:<22} {:>9} {:>14} {:>14} {:>14} {:>14}",
        "part", "raw MB", "zstd 3", "zstd 5", "zstd 7", "zstd 9"
    );
    for (name, data) in &parts {
        let cells: Vec<String> = [(3, false), (5, false), (7, false), (9, false)]
            .iter()
            .map(|&(l, long)| {
                let (size, ms) = zsize(data, l, long);
                format!("{:.2} ({:.0}ms)", size as f64 / 1048576.0, ms)
            })
            .collect();
        println!(
            "{:<22} {:>9.2} {:>14} {:>14} {:>14} {:>14}",
            name,
            data.len() as f64 / 1048576.0,
            cells[0],
            cells[1],
            cells[2],
            cells[3]
        );
    }
}
