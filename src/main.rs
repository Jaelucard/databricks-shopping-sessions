mod aggregate;
mod dedup;
mod input;
mod keys;
mod output;
mod parse;

use input::{Catalog, Parts};
use keys::{Key, Keys};
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use std::{env, fs, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const DAY: i64 = 86_400_000;
const BUCKETS: usize = 64;
const BLOCK: usize = 2 << 20;

#[derive(Clone, Copy)]
struct Event {
    person: Key,
    ts: i64,
    price: u32,
    category: u32,
}
#[derive(Clone, Copy)]
struct Click {
    person: Key,
    ts: i64,
    id: Key,
    campaign: u32,
}
struct UserDay {
    person: Key,
    day: i32,
    sessions: i64,
    events: i64,
    revenue: i64,
    top: Option<u32>,
}
#[derive(Default)]
struct CampaignDay {
    campaign: u32,
    day: i32,
    sessions: i64,
    purchasing: i64,
    revenue: i64,
    assisted: i64,
}
#[derive(Clone, Copy)]
struct Candidate {
    id: Key,
    ingest: i64,
    event: Event,
}

fn bucket(key: Key) -> usize {
    (key.wrapping_mul(0x9e3779b97f4a7c15) >> (64 - BUCKETS.trailing_zeros())) as usize
}
fn bins<T>(approx_per_bucket: usize) -> Vec<Vec<T>> {
    (0..BUCKETS)
        .map(|_| Vec::with_capacity(approx_per_bucket))
        .collect()
}
fn group_buckets<T>(parts: Vec<Vec<Vec<T>>>) -> Vec<Vec<Vec<T>>> {
    let mut columns = bins(parts.len());
    for part in parts {
        for (col, values) in columns.iter_mut().zip(part) {
            col.push(values);
        }
    }
    columns
}
fn flatten<T>(pieces: Vec<Vec<T>>) -> Vec<T> {
    let mut flat = Vec::with_capacity(pieces.iter().map(Vec::len).sum());
    for mut piece in pieces {
        flat.append(&mut piece);
    }
    flat
}

#[inline]
fn event_row(row: &[u8], cat: &Catalog, keys: &Keys) -> Option<Candidate> {
    let [id, user, ts, ingest, kind, product, price] = parse::fields(row)?;
    if id.is_empty() {
        return None;
    }
    let id = keys.key(id);
    let ingest = parse::ingest_timestamp(ingest)?;
    let event = (|| {
        if user.is_empty() || product.is_empty() {
            return None;
        }
        let ts = parse::timestamp(ts)?;
        if ts < cat.start || ts >= cat.end || ingest - ts > DAY {
            return None;
        }
        let (price, category) = match kind {
            b"view" | b"cart" => (0, 0),
            b"purchase" => (
                parse::price(price)?,
                cat.products
                    .get(&keys.key(product))
                    .copied()
                    .unwrap_or(cat.unknown),
            ),
            _ => return None,
        };
        Some(Event {
            person: keys.key(user),
            ts,
            price,
            category,
        })
    })()
    .unwrap_or(Event {
        person: 0,
        ts: 0,
        price: 0,
        category: 0,
    });
    // Keep an invalid winner as a tombstone until every copy of this ID has met.
    Some(Candidate { id, ingest, event })
}

fn ingest_events(dir: &Path, cat: &Catalog, keys: &Keys) -> Result<Vec<Vec<Vec<Candidate>>>> {
    let files = Parts::read(dir)?;
    let batches = files
        .chunks(BLOCK)
        .into_par_iter()
        .map(|bytes| {
            let mut parts = bins(bytes.len() / 75 / BUCKETS + 1);
            input::lines(bytes, |line| {
                if let Some(row) = event_row(line, cat, keys) {
                    parts[bucket(row.id)].push(row)
                }
            });
            parts
        })
        .collect();
    Ok(group_buckets(batches))
}

fn ingest_clicks(
    dir: &Path,
    cat: &Catalog,
    keys: &Keys,
    aliases: &FxHashMap<Key, Key>,
) -> Result<Vec<Vec<Vec<Click>>>> {
    let files = Parts::read(dir)?;
    let start = cat.start - 7 * DAY;
    let batches = files
        .chunks(BLOCK)
        .into_par_iter()
        .map(|bytes| {
            let mut parts = bins(bytes.len() / 48 / BUCKETS + 1);
            input::lines(bytes, |line| {
                let Some([id, user, campaign, ts]) = parse::fields(line) else {
                    return;
                };
                if id.is_empty() || user.is_empty() {
                    return;
                }
                let Some(ts) = parse::timestamp(ts) else {
                    return;
                };
                if ts < start || ts >= cat.end {
                    return;
                }
                let Some(&campaign) = cat.campaign_ids.get(&keys.key(campaign)) else {
                    return;
                };
                let raw = keys.key(user);
                let person = aliases.get(&raw).copied().unwrap_or(raw);
                parts[bucket(person)].push(Click {
                    person,
                    ts,
                    id: keys.key(id),
                    campaign,
                });
            });
            parts
        })
        .collect();
    Ok(group_buckets(batches))
}

fn run(data: &Path, out: &Path) -> Result<()> {
    let trace = env::var_os("PROFILE_STAGES").is_some();
    let mut start = Instant::now();
    let mut mark = |label: &str| {
        if trace {
            eprintln!("{label}: {:.3}s", start.elapsed().as_secs_f64());
        }
        start = Instant::now();
    };
    let keys = Keys::default();
    let cat = input::catalog(data, &keys)?;
    mark("dimensions");
    let (raw, linked_clicks) = rayon::join(
        || ingest_events(&data.join("events"), &cat, &keys),
        || -> Result<_> {
            let aliases = input::people(&data.join("links"), &keys)?;
            let clicks = ingest_clicks(&data.join("clicks"), &cat, &keys, &aliases)?;
            Ok((aliases, clicks))
        },
    );
    let raw = raw?;
    let (aliases, clicks) = linked_clicks?;
    mark("links, parse and partition");
    let events = group_buckets(
        raw.into_par_iter()
            .map(|parts| dedup::winners(parts, &aliases))
            .collect(),
    );
    drop(aliases);
    mark("event dedup and repartition");
    let totals: Vec<_> = events
        .into_par_iter()
        .zip(clicks)
        .map(|(events, clicks)| {
            aggregate::aggregate(flatten(events), flatten(clicks), &keys, cat.campaigns.len())
        })
        .collect();
    mark("sort and aggregate");

    let users = out.join("user_daily");
    let campaigns = out.join("campaign_daily");
    fs::create_dir_all(&users)?;
    fs::create_dir_all(&campaigns)?;
    let mut parts: Vec<Vec<Vec<UserDay>>> = (0..8).map(|_| Vec::new()).collect();
    let mut counts = FxHashMap::<(i32, u32), CampaignDay>::default();
    for (i, (user, rows)) in totals.into_iter().enumerate() {
        parts[i % 8].push(user);
        for row in rows {
            let sum = counts
                .entry((row.day, row.campaign))
                .or_insert_with(|| CampaignDay {
                    day: row.day,
                    campaign: row.campaign,
                    ..Default::default()
                });
            sum.sessions += row.sessions;
            sum.purchasing += row.purchasing;
            sum.revenue += row.revenue;
            sum.assisted += row.assisted;
        }
    }
    let (a, b) = rayon::join(
        || {
            parts
                .into_par_iter()
                .enumerate()
                .try_for_each(|(i, chunks)| {
                    output::write_users(
                        &users.join(format!("part-{i}.parquet")),
                        &flatten(chunks),
                        &keys,
                        &cat.categories,
                    )
                })
        },
        || {
            output::write_campaigns(
                &campaigns.join("part-0.parquet"),
                &counts.into_values().collect::<Vec<_>>(),
                &cat.campaigns,
            )
        },
    );
    a?;
    b?;
    mark("merge and parquet");
    Ok(())
}

fn main() -> Result<()> {
    let n: usize = env::var("NCPU").unwrap_or_else(|_| "8".into()).parse()?;
    if n == 0 {
        return Err("NCPU must be positive".into());
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .build_global()?;
    let data = env::var_os("DATA_DIR").unwrap_or_else(|| "/data".into());
    let output = env::var_os("OUTPUT_DIR").unwrap_or_else(|| "/output".into());
    run(Path::new(&data), Path::new(&output))
}
