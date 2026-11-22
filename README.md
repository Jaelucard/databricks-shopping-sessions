<img src="assets/icon.svg" width="64" height="64" alt="">

# Databricks - Shopping Sessions

This Rust executable reads the mounted CSV inputs and produces `user_daily` and `campaign_daily` Parquet tables. The production image starts the binary directly and uses eight worker threads by default. Set `DATA_DIR`, `OUTPUT_DIR`, and `NCPU` to run it locally. Each run expects an empty output directory.

## Build and verify

Download the official Polars starter, generator, and public dev answer from the [Shopping Sessions track](https://www.joinbuilders.org/buildathon/tracks/shopping-sessions). Extract them outside this repository. The fixture scripts require Python with `polars==2.1.0`. Build the generator with `cargo build --release --locked` in its directory. Use a local virtual environment for the checker.

The Makefile accepts `PYTHON`, `FIXTURES`, `GENERATE`, `DEV`, `ANSWER`, and `OUT` as overrides. An optional ignored `local.mk` can point them to local tools and datasets. Use `OUT` for a fresh directory dedicated to one dataset. Keep expected CSVs in `ANSWER` because the organizer’s comparison script also accepts top-level CSVs as input. On a configured checkout, run

```sh
make dev
make fixtures
make run-dev
make check-dev
```

`make fixtures` and `make run-dev` build the linux/amd64 image and invoke the organizer's container harness. For native development, use `make fixtures-native run-dev-native` and then `make check-dev`. Run the Rust tests with `cargo test --locked`.

The independent reference in `scripts/reference.py` is for small inputs. The following compares Parquet output to it on three fresh seeds. Use the Python environment containing Polars.

```sh
python scripts/differential.py seeds --generator /path/to/generate \
  --pipeline target/release/shopping-sessions --seeds 2 17 103
```

## Benchmark

Generate the input once before timing. The public dev ratio is 4 events to 1 click. A local practice proxy can be generated with

```sh
generate --seed 17 --events 20000000 --clicks 5000000 --output /path/to/practice
```

For a native process benchmark, run

```sh
python3 scripts/benchmark.py --data /path/to/practice --binary target/release/shopping-sessions \
  --output /path/to/new-native-results --runs 3
```

Use `--image shopping-sessions:latest` in place of `--binary` for linux/amd64 Docker measurements. The Docker harness pins eight logical CPUs and times each container from its engine-reported start to finish. To time an input already staged on the Docker VM’s local disk, pass `--volume VOLUME` in place of `--data PATH`. Set `--memory 7g` on an undersized local Docker VM. Each run keeps its tables for subsequent exact comparison. The official scorer controls its own hardware and cache conditions.

## How the pipeline works

Line-aligned chunks are parsed in parallel. Short ASCII IDs pack losslessly into integers and arbitrary UTF-8 IDs use a byte-exact interner. Link components choose their smallest original ID. Raw event parsing can proceed while the link graph is built because global event selection is independent of identity resolution.

Event parsing keeps payloads in input order. Each chunk groups 16-byte records containing the event ID, ingest time and original row position into one contiguous allocation. Workers deduplicate those records by ID. A two-bit filter proves which IDs appeared once. An exact map selects the earliest ingest for the remaining IDs, including invalid winners. Workers return the losing row positions, and each surviving payload goes directly to its canonical person’s partition.

For each person, two cursors maintain the inclusive seven-day click window. An active-campaign list counts each assisting campaign once. The latest timestamp and smallest original click ID determine credit. Identical click copies have the same lifetime and campaign and therefore leave both outputs unchanged. Day and purchase category totals use the session's UTC start date.

[Local 20M benchmark results](BENCHMARKS.md)

## License & Credits

This project is released under the [MIT License](LICENSE).

Open-source dependencies and resources used:
- [memchr](https://github.com/BurntSushi/memchr), [memmap2](https://github.com/RazrFalcon/memmap2-rs), [parquet](https://github.com/apache/arrow-rs), [rayon](https://github.com/rayon-rs/rayon), [rustc-hash](https://github.com/rust-lang/rustc-hash), [serde](https://github.com/serde-rs/serde), and [serde_json](https://github.com/serde-rs/json) (see [`Cargo.toml`](Cargo.toml) and [`Cargo.lock`](Cargo.lock))
- Builders Buildathon Shopping Sessions track specification and starter resources ([joinbuilders.org](https://www.joinbuilders.org/buildathon/tracks/shopping-sessions))

Created in [T3 Code](https://t3.codes).
