use super::{BUCKETS, Candidate, Event, bins, bucket};
use crate::Key;
use rustc_hash::FxHashMap;

#[inline(always)]
fn slot_hash(key: u64) -> u64 {
    let mixed = (key ^ (key >> 32)).wrapping_mul(0x9e3779b97f4a7c15);
    mixed ^ (mixed >> 32)
}

#[inline]
fn emit(mut event: Event, out: &mut [Vec<Event>], aliases: &FxHashMap<Key, Key>) {
    if event.person != 0 {
        event.person = aliases.get(&event.person).copied().unwrap_or(event.person);
        out[bucket(event.person)].push(event);
    }
}

/// A fingerprint only rejects unequal IDs. Confirm every potential match with
/// the full key and retain its earliest ingest even if that event is invalid.
fn resolve_ambiguous(
    mut rows: Vec<Candidate>,
    out: &mut [Vec<Event>],
    aliases: &FxHashMap<Key, Key>,
) {
    if rows.is_empty() {
        return;
    }
    if rows.len() > u32::MAX as usize {
        rows.sort_unstable_by_key(|r| (r.id, r.ingest));
        rows.dedup_by_key(|r| r.id);
    } else {
        let capacity = (rows.len() + rows.len() / 2 + 1).next_power_of_two();
        let mut slots = vec![0_u64; capacity];
        let mask = capacity - 1;
        for index in 0..rows.len() {
            let id = rows[index].id;
            let hash = slot_hash(id);
            // A nonzero tag occupies the top 32 bits; the low 32 bits address rows.
            let tag = (hash | (1 << 63)) & 0xffff_ffff_0000_0000;
            let mut slot = hash as usize & mask;
            loop {
                let entry = slots[slot];
                if entry == 0 {
                    slots[slot] = tag | index as u64;
                    break;
                }
                if entry & 0xffff_ffff_0000_0000 == tag {
                    let old = entry as u32 as usize;
                    if rows[old].id == id {
                        if rows[old].ingest <= rows[index].ingest {
                            rows[index].event.person = 0;
                        } else {
                            rows[old].event.person = 0;
                            slots[slot] = tag | index as u64;
                        }
                        break;
                    }
                }
                slot = (slot + 1) & mask;
            }
        }
    }
    for row in rows {
        emit(row.event, out, aliases);
    }
}

pub(super) fn winners(
    pieces: Vec<Vec<Candidate>>,
    aliases: &FxHashMap<Key, Key>,
) -> Vec<Vec<Event>> {
    let count: usize = pieces.iter().map(Vec::len).sum();
    let slots = (count * 16).max(64).next_power_of_two();
    let mask = slots - 1;
    let mut filter = vec![[0_u64; 2]; slots / 64];
    for piece in &pieces {
        for row in piece {
            let pos = slot_hash(row.id) as usize & mask;
            let bit = 1_u64 << (pos & 63);
            let entry = &mut filter[pos >> 6];
            entry[1] |= entry[0] & bit;
            entry[0] |= bit;
        }
    }
    let mut out = bins(count / BUCKETS + 1);
    let mut ambiguous = Vec::with_capacity(count / 5);
    for piece in pieces {
        for row in piece {
            let pos = slot_hash(row.id) as usize & mask;
            if ((filter[pos >> 6][1] >> (pos & 63)) & 1) == 0 {
                // Exactly one row mapped to this bit, proving that its ID is unique.
                emit(row.event, &mut out, aliases);
            } else {
                ambiguous.push(row);
            }
        }
    }
    drop(filter);
    resolve_ambiguous(ambiguous, &mut out, aliases);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn row(id: u64, ingest: i64, person: u64) -> Candidate {
        Candidate {
            id,
            ingest,
            event: Event {
                person,
                ts: ingest + 5,
                price: 42,
                category: 2,
            },
        }
    }

    fn check(pieces: Vec<Vec<Candidate>>) {
        let mut reference = BTreeMap::<u64, &Candidate>::new();
        for row in pieces.iter().flatten() {
            let best = reference.entry(row.id).or_insert(row);
            if row.ingest < best.ingest {
                *best = row;
            }
        }
        let as_tuple = |event: Event| (event.person, event.ts, event.price, event.category);
        let mut expected: Vec<_> = reference
            .values()
            .filter(|r| r.event.person != 0)
            .map(|r| as_tuple(r.event))
            .collect();
        expected.sort_unstable();
        let mut actual: Vec<_> = winners(pieces, &FxHashMap::default())
            .into_iter()
            .flatten()
            .map(as_tuple)
            .collect();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn earliest_copy_controls_the_entire_event() {
        check(vec![
            vec![],
            vec![row(3, 12, 25), row(4, -5, 0), row(7, 10, 30)],
            vec![row(3, 15, 0), row(4, 2, 9), row(7, 1, 14), row(7, 1, 14)],
        ]);
    }

    #[test]
    fn exact_id_wins_over_equal_hash_tag_and_slot() {
        // These hashes differ only in the tag bit forced high to reserve zero.
        const INV: u64 = 0xf1de83e19937733d;
        let inverse = |hash: u64| {
            let mixed = (hash ^ (hash >> 32)).wrapping_mul(INV);
            mixed ^ (mixed >> 32)
        };
        let a = inverse(0);
        let b = inverse(1 << 63);
        assert_eq!(slot_hash(a), 0);
        assert_eq!(slot_hash(b), 1 << 63);
        check(vec![
            vec![row(a, 2, 2), row(b, 3, 3)],
            vec![row(a, 1, 0), row(b, 1, 5)],
        ]);
        check(vec![Vec::new(), Vec::new()]);
    }
}
