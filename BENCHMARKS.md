# 20M-event benchmarks

Measured locally on November 21, 2026. Each input contains 20,000,000 requested events and 5,000,000 requested clicks, plus the generator’s duplicates, links, dimensions, and malformed rows. Seed 17 contained 1,921,192,554 bytes. Seed 73 contained 1,870,234,737 bytes.

| Final build and input | Run 1 | Run 2 | Run 3 | Median |
| --- | ---: | ---: | ---: | ---: |
| Apple M2 Pro, native process, seed 17 | 1.181 s | 1.172 s | 1.530 s | 1.181 s |
| Apple M2 Pro, native process, seed 73 | 1.915 s | 1.127 s | 1.173 s | 1.173 s |
| linux/amd64 Docker emulation, VM volume, seed 17 | 3.938 s | 3.190 s | 2.340 s | 3.190 s |
| linux/amd64 Docker emulation, Mac bind mount, seed 17 | 7.548 s | 5.341 s | 4.681 s | 5.341 s |

The native times span process launch through exit with eight Rayon workers. Docker times use the engine’s container start and finish timestamps. Each run starts a fresh process or container and writes both Parquet tables to a fresh directory. The Docker input volume was populated before timing. Docker output was written to a Mac bind mount in both cases. Host and VM caches were allowed to respond normally between runs. These results measure this computer. The track’s 0.65-second practice target uses a different machine.

## Optimization evidence

In a separate interleaved seed-17 native comparison, the first correct scalar build took a median of **1.952 s** and the selected build took **1.380 s**, a **29.3%** reduction. A parallel checksum touched every input byte immediately before each run. Each variant reused its dedicated output path. Those conditions differ from the fresh-output series above.

The selected version scans CSV delimiters with NEON on arm64 or runtime-dispatched AVX2/SSE2 on x86-64, packs short IDs without losing their bytes, and overlaps link construction with raw event parsing. A per-partition filter proves which event IDs are unique. Potential collisions and duplicate IDs go through exact ID and ingest comparisons. The remaining events are sorted by canonical person and timestamp, and a sliding click window computes credit and assists.

In a separate three-way Docker comparison on VM-volume input, plain memory mapping took a median of 2.933 s, mapping with an advisory read-ahead hint took 2.512 s, and bulk reads into owned buffers took 4.838 s. On the Mac bind mount, the corresponding medians were 6.223 s, 6.364 s, and 7.346 s. The image uses mapped files with the read-ahead hint. Every row still passes through structural validation, cleaning, sessionization, and attribution.

## Correctness and limits

- The final executable passed all 36 official fixtures both natively and in the linux/amd64 submission image. The public 1M seed-1 comparison matched every one of the 117,038 user rows and 27,400 campaign rows and found zero duplicate keys.
- Five generated 5,000-event inputs matched an independent Python reference. At 20M, both tables on seeds 17 and 73 matched the initial passing scalar implementation. Seed 17 produced 2,249,563 user/day rows and 28,014 campaign/day rows. Seed 73 produced 2,230,619 and 28,014. The organizer supplies an answer for seed 1 at 1M.
- Docker Desktop used an arm64 Linux VM with about 7.75 GiB available. Each benchmark container had eight pinned virtual CPUs, a 7 GiB memory limit, disabled swap and network, uid 1000, a read-only root and input, and a 64 MiB temporary filesystem. One additional seed-17 volume run recorded a cgroup memory peak of 1,695,920,128 bytes (1.58 GiB), 98,143,989 output bytes, and zero scratch bytes.
- Docker reported an image size of 115,577,457 bytes. A fresh Rust compilation inside the amd64 image build took 56.6 s end to end with the base images and Cargo download cache available.
- The official hidden practice score and 60M final-size runtime remain unmeasured. The README gives the commands to reproduce these local tests.
