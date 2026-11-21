use super::{BUCKETS, Event, Key, bins, bucket, group_buckets};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::collections::hash_map::Entry;

pub(super) const SHARDS: usize = 256;
const ROW_BITS: u32 = 25;
const ROW_MASK: i64 = (1_i64 << ROW_BITS) - 1;

/// Only metadata crosses the event-ID partition. Keeping the event payload in
/// input order lets survivors route directly to their canonical-person bucket.
#[derive(Clone, Copy)]
pub(super) struct IdRecord {
    id: Key,
    ingest_and_row: i64,
}

impl IdRecord {
    #[inline]
    pub fn new(id: Key, ingest_ms: i64, local_row: usize) -> Self {
        // A 2 MiB chunk ends at the next LF, so it contains at most 2 MiB + 1
        // physical lines. Four-digit F1 civil years fit in a signed 39-bit
        // Unix-second field, including every supported date before 1970.
        assert!(
            local_row <= ROW_MASK as usize,
            "event offset exceeds its 25-bit field"
        );
        let seconds = ingest_ms / 1000;
        assert!(
            (i64::MIN >> ROW_BITS..=i64::MAX >> ROW_BITS).contains(&seconds),
            "ingest second exceeds its signed 39-bit field"
        );
        Self {
            id,
            ingest_and_row: seconds << ROW_BITS | local_row as i64,
        }
    }
    #[inline]
    fn row(self) -> usize {
        (self.ingest_and_row & ROW_MASK) as usize
    }
}

#[inline(always)]
pub(super) fn shard(id: Key) -> usize {
    (id.wrapping_mul(0x9e3779b97f4a7c15) >> (64 - SHARDS.trailing_zeros())) as usize
}

/// The input chunk owns one stable, contiguous range for each ID shard.
pub(super) struct ChunkMeta {
    records: Vec<IdRecord>,
    ends: [usize; SHARDS],
}

impl ChunkMeta {
    pub(super) fn scatter(records: Vec<IdRecord>, mut counts: [usize; SHARDS]) -> Self {
        let mut total = 0;
        for count in &mut counts {
            let len = *count;
            *count = total;
            total += len;
        }
        debug_assert_eq!(total, records.len());
        let mut slab = vec![
            IdRecord {
                id: 0,
                ingest_and_row: 0
            };
            total
        ];
        for record in records {
            let pos = &mut counts[shard(record.id)];
            slab[*pos] = record;
            *pos += 1;
        }
        // Each cursor has advanced from that shard's start to its exclusive end.
        Self {
            records: slab,
            ends: counts,
        }
    }

    fn slice(&self, shard: usize) -> &[IdRecord] {
        let start = if shard == 0 { 0 } else { self.ends[shard - 1] };
        &self.records[start..self.ends[shard]]
    }
}

#[inline(always)]
fn filter_hash(id: Key) -> usize {
    let mixed = (id ^ (id >> 32)).wrapping_mul(0x9e3779b97f4a7c15);
    (mixed ^ (mixed >> 32)) as usize
}

/// All records for an ID land in the same shard. The table holds only the
/// original ID and a position in the existing metadata, not the event payload.
fn losing_rows(pieces: &[&[IdRecord]]) -> Vec<usize> {
    assert!(
        pieces.len() <= (usize::MAX >> ROW_BITS),
        "too many source chunks to encode a position"
    );
    let size = pieces.iter().map(|piece| piece.len()).sum::<usize>();
    let slots = (size * 16).max(64).next_power_of_two();
    let mask = slots - 1;
    // A bit set once proves that its only record cannot lose. A second hit
    // can be a duplicate or a hash collision; compare full IDs for those.
    let mut filter = vec![[0_u64; 2]; slots / 64];
    for record in pieces.iter().copied().flatten() {
        let pos = filter_hash(record.id) & mask;
        let bit = 1_u64 << (pos & 63);
        let block = &mut filter[pos >> 6];
        block[1] |= block[0] & bit;
        block[0] |= bit;
    }
    let ambiguous_slots: usize = filter.iter().map(|b| b[1].count_ones() as usize).sum();
    if ambiguous_slots == 0 {
        return Vec::new();
    }
    let mut winners = FxHashMap::<Key, usize>::default();
    winners.reserve((2 * ambiguous_slots).min(size));
    let mut rejected = Vec::with_capacity(size / 16);
    for (chunk, records) in pieces.iter().enumerate() {
        let block = chunk << ROW_BITS;
        for (offset, &record) in records.iter().enumerate() {
            let filter_pos = filter_hash(record.id) & mask;
            if filter[filter_pos >> 6][1] & (1_u64 << (filter_pos & 63)) == 0 {
                continue;
            }
            // A per-shard record index is no larger than the local event row
            // already checked by IdRecord::new.
            let position = block | offset;
            match winners.entry(record.id) {
                Entry::Vacant(entry) => {
                    entry.insert(position);
                }
                Entry::Occupied(mut entry) => {
                    let previous = *entry.get();
                    let prior_block = previous >> ROW_BITS;
                    let prior = pieces[prior_block][previous & ROW_MASK as usize];
                    // Equal ingest times have identical payloads. Row bits can
                    // choose either one without changing the result.
                    if prior.ingest_and_row <= record.ingest_and_row {
                        rejected.push(block | record.row());
                    } else {
                        rejected.push((prior_block << ROW_BITS) | prior.row());
                        entry.insert(position);
                    }
                }
            }
        }
    }
    rejected
}

pub(super) fn partition(
    mut chunks: Vec<Vec<Event>>,
    metadata: Vec<ChunkMeta>,
    aliases: &FxHashMap<Key, Key>,
) -> Vec<Vec<Vec<Event>>> {
    let losing: Vec<_> = (0..SHARDS)
        .into_par_iter()
        .map(|shard| {
            let pieces: Vec<_> = metadata.iter().map(|chunk| chunk.slice(shard)).collect();
            losing_rows(&pieces)
        })
        .collect();
    drop(metadata);
    let mut by_chunk = vec![Vec::new(); chunks.len()];
    for group in losing {
        for position in group {
            by_chunk[position >> ROW_BITS].push((position & ROW_MASK as usize) as u32);
        }
    }

    let outputs = chunks
        .par_chunks_mut(4)
        .zip(by_chunk.par_chunks(4))
        .map(|(batch, losers)| {
            let count = batch.iter().map(Vec::len).sum::<usize>();
            let mut out = bins(count / BUCKETS + 1);
            for (chunk, rejected) in batch.iter_mut().zip(losers) {
                for &row in rejected {
                    chunk[row as usize].person = 0;
                }
                for mut event in std::mem::take(chunk) {
                    if event.person != 0 {
                        event.person = aliases.get(&event.person).copied().unwrap_or(event.person);
                        out[bucket(event.person)].push(event);
                    }
                }
            }
            out
        })
        .collect();
    group_buckets(outputs)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn chunk_meta(records: Vec<IdRecord>) -> ChunkMeta {
        let mut counts = [0; SHARDS];
        for record in &records {
            counts[shard(record.id)] += 1;
        }
        ChunkMeta::scatter(records, counts)
    }

    #[test]
    fn scatter_preserves_chunk_order_and_empty_shards() {
        for ids in [vec![], vec![7, 12, 7, 7, 13, 12, 99]] {
            let slab = chunk_meta(
                ids.iter()
                    .enumerate()
                    .map(|(i, &id)| IdRecord::new(id, 0, i))
                    .collect(),
            );
            for bucket in 0..SHARDS {
                let actual: Vec<_> = slab.slice(bucket).iter().map(|r| (r.id, r.row())).collect();
                let expected: Vec<_> = ids
                    .iter()
                    .enumerate()
                    .filter(|(_, id)| shard(**id) == bucket)
                    .map(|(i, &id)| (id, i))
                    .collect();
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn selects_earliest_copy_across_chunks_and_signed_epochs() {
        let pieces: &[&[IdRecord]] = &[
            &[
                IdRecord::new(7, 0, 4),
                IdRecord::new(1, 8000, 2),
                IdRecord::new(99, -62_167_219_200_000, 12),
            ],
            &[
                IdRecord::new(1, -1000, 7),
                IdRecord::new(7, 1000, 1),
                IdRecord::new(99, 253_402_300_799_000, 28),
            ],
        ];
        let mut rejected = losing_rows(pieces);
        rejected.sort_unstable();
        assert_eq!(rejected, vec![2, (1 << ROW_BITS) | 1, (1 << ROW_BITS) | 28]);
    }

    #[test]
    fn colliding_distinct_ids_are_never_rejected_as_duplicates() {
        let id = 7;
        let collision = (id + 1..)
            .find(|&other| (filter_hash(other) ^ filter_hash(id)) & 63 == 0)
            .unwrap();
        let pieces: &[&[IdRecord]] = &[
            &[
                IdRecord::new(id, 1000, 3),
                IdRecord::new(collision, 2000, 4),
            ],
            &[IdRecord::new(id, 0, 9)],
        ];
        assert_eq!(losing_rows(pieces), vec![3]);
    }

    #[test]
    fn invalid_earliest_and_identical_ingest_ties_in_other_chunks() {
        let event = |person| Event {
            person,
            ts: 8000,
            price: 0,
            category: 0,
        };
        let chunks = vec![vec![event(74), event(7)], vec![event(0), event(7)]];
        let records = vec![
            chunk_meta(vec![IdRecord::new(1, 2000, 0), IdRecord::new(7, 3000, 1)]),
            chunk_meta(vec![IdRecord::new(1, 1000, 0), IdRecord::new(7, 3000, 1)]),
        ];
        let output = partition(chunks, records, &FxHashMap::default());
        let winners: Vec<_> = output
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.person)
            .collect();
        assert_eq!(winners, vec![7]);
    }
}
