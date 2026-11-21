use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

use memchr::memchr;
use memmap2::Mmap;
use rayon::prelude::*;
use rustc_hash::FxHashMap;
use serde::Deserialize;

use crate::{Key, Keys, Result, parse};

pub struct Parts(Vec<Mmap>);

impl Parts {
    pub fn read(dir: &Path) -> Result<Self> {
        let mut paths = Vec::<PathBuf>::new();
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "csv") {
                paths.push(path);
            }
        }
        let mapped: io::Result<Vec<_>> = paths
            .par_iter()
            .map(|path| {
                let file = File::open(path)?;
                if file.metadata()?.len() == 0 {
                    return Ok(None);
                }
                // Input files are immutable for the duration of a run (read-only mount).
                let mapping = unsafe { Mmap::map(&file) }?;
                // A filesystem may decline this read-ahead hint.
                #[cfg(unix)]
                let _ = mapping.advise(memmap2::Advice::WillNeed);
                Ok(Some(mapping))
            })
            .collect();
        Ok(Self(mapped?.into_iter().flatten().collect()))
    }

    pub fn chunks(&self, target: usize) -> Vec<&[u8]> {
        let mut chunks = Vec::new();
        for part in &self.0 {
            let Some(header_end) = memchr(b'\n', part) else {
                continue;
            };
            let mut start = header_end + 1;
            while start < part.len() {
                let split = (start + target).min(part.len());
                let end = memchr(b'\n', &part[split..]).map_or(part.len(), |n| split + n + 1);
                chunks.push(&part[start..end]);
                start = end;
            }
        }
        chunks
    }
}

#[inline]
pub fn lines(mut bytes: &[u8], mut visit: impl FnMut(&[u8])) {
    while let Some(pos) = memchr(b'\n', bytes) {
        visit(&bytes[..pos]);
        bytes = &bytes[pos + 1..];
    }
    if !bytes.is_empty() {
        visit(bytes)
    }
}

pub struct Catalog {
    pub start: i64,
    pub end: i64,
    pub products: FxHashMap<Key, u32>,
    pub categories: Vec<String>,
    pub unknown: u32,
    pub campaign_ids: FxHashMap<Key, u32>,
    pub campaigns: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct Window {
    window_start: String,
    window_end: String,
}

fn dimension(path: &Path) -> Result<Vec<(String, String)>> {
    let bytes = fs::read(path)?;
    let mut result = Vec::new();
    for line in bytes
        .split(|&b| b == b'\n')
        .skip(1)
        .filter(|l| !l.is_empty())
    {
        let [a, b] = parse::fields::<2>(line).ok_or("malformed lookup row")?;
        result.push((
            std::str::from_utf8(a)?.into(),
            std::str::from_utf8(b)?.into(),
        ));
    }
    Ok(result)
}

struct AliasMap<'a> {
    keys: &'a Keys,
    parent: FxHashMap<Key, Key>,
}
impl AliasMap<'_> {
    #[inline]
    fn root(&mut self, mut key: Key) -> (Key, bool) {
        let Some(mut next) = self.parent.get(&key).copied() else {
            return (key, false);
        };
        while next != key {
            let grandparent = self.parent[&next];
            if grandparent == next {
                return (next, true);
            }
            self.parent.insert(key, grandparent);
            key = grandparent;
            next = self.parent[&key];
        }
        (key, true)
    }
    fn join(&mut self, a: Key, b: Key) {
        let (a, known_a) = self.root(a);
        let (b, known_b) = self.root(b);
        let (winner, loser, known) = if self.keys.cmp(a, b).is_le() {
            (a, b, known_a)
        } else {
            (b, a, known_b)
        };
        if !known {
            self.parent.insert(winner, winner);
        }
        if winner != loser {
            self.parent.insert(loser, winner);
        }
    }
    fn finish(mut self) -> FxHashMap<Key, Key> {
        // Flatten indirect links; roots point to themselves.
        let indirect: Vec<_> = self
            .parent
            .iter()
            .filter(|(_, v)| self.parent.get(*v) != Some(*v))
            .map(|(&k, &v)| (k, v))
            .collect();
        for (k, v) in indirect {
            let (root, _) = self.root(v);
            self.parent.insert(k, root);
        }
        self.parent
    }
}

pub fn people(dir: &Path, keys: &Keys) -> Result<FxHashMap<Key, Key>> {
    let links = Parts::read(dir)?;
    let chunks = links.chunks(256 << 10);
    let blocks: Vec<Vec<(Key, Key)>> = chunks
        .par_iter()
        .map(|chunk| {
            let mut edges = Vec::with_capacity(chunk.len() / 20);
            lines(chunk, |line| {
                if let Some([a, b]) = parse::fields::<2>(line)
                    && !a.is_empty()
                    && !b.is_empty()
                {
                    edges.push((keys.key(a), keys.key(b)));
                }
            });
            edges
        })
        .collect();
    let mut parent = FxHashMap::default();
    parent.reserve((blocks.iter().map(Vec::len).sum::<usize>() * 3 / 2).min(5_000_000));
    let mut aliases = AliasMap { keys, parent };
    for edges in blocks {
        for (a, b) in edges {
            aliases.join(a, b);
        }
    }
    Ok(aliases.finish())
}
type ProductLookup = (FxHashMap<Key, u32>, Vec<String>, u32);

fn products(file: &Path, keys: &Keys) -> Result<ProductLookup> {
    let buf = std::fs::read(file)?;
    let mut seen = rustc_hash::FxHashSet::default();
    seen.insert(b"unknown".as_slice());
    let mut pairs = Vec::with_capacity((buf.len() / 16).min(1000000));
    let mut rows = buf.split(|&b| b == b'\n');
    rows.next();
    for row in rows.filter(|r| !r.is_empty()) {
        let [p, c] = parse::fields::<2>(row).ok_or("malformed lookup row")?;
        seen.insert(c);
        pairs.push((keys.key(p), c));
    }
    let mut cats: Vec<_> = seen.into_iter().collect();
    cats.sort_unstable();
    let ranked: FxHashMap<_, _> = cats
        .iter()
        .enumerate()
        .map(|(i, &c)| (c, i as u32))
        .collect();
    let unknown = ranked[b"unknown".as_slice()];
    let products = pairs.into_iter().map(|(p, c)| (p, ranked[c])).collect();
    let cats = cats
        .into_iter()
        .map(|s| std::str::from_utf8(s).map(str::to_owned))
        .collect::<std::result::Result<_, _>>()?;
    Ok((products, cats, unknown))
}
pub fn catalog(dir: &Path, keys: &Keys) -> Result<Catalog> {
    let win: Window = serde_json::from_slice(&fs::read(dir.join("manifest.json"))?)?;
    let (products, categories, unknown) = products(&dir.join("products.csv"), keys)?;
    let mut campaigns = vec![("organic".into(), "organic".into())];
    campaigns.extend(dimension(&dir.join("campaigns.csv"))?);
    let campaign_ids = campaigns
        .iter()
        .enumerate()
        .skip(1)
        .map(|(i, (c, _))| (keys.key(c.as_bytes()), i as u32))
        .collect();
    Ok(Catalog {
        start: parse::timestamp(win.window_start.as_bytes()).ok_or("bad window start")?,
        end: parse::timestamp(win.window_end.as_bytes()).ok_or("bad window end")?,
        categories,
        unknown,
        products,
        campaign_ids,
        campaigns,
    })
}
