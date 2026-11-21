# 20M-event performance

Measured on November 21, 2026. The submission target is **0.65 s**. Both public-generator inputs requested 20M events and 5M clicks. Including duplicates, dimensions and links, seed 17 is 1,921,192,554 bytes and seed 73 is 1,870,234,737 bytes.

## Selected build

| Warm input, fresh process or container | Seed 17 median (s) | Seed 73 median (s) |
| --- | ---: | ---: |
| Native Apple M2 Pro, previous pipeline | 1.068 | 1.369 |
| Native Apple M2 Pro, selected pipeline | 0.800 | 0.845 |
| amd64 Docker, previous executable in the minimal runtime | 2.336 | 1.865 |
| amd64 Docker, selected executable in the minimal runtime | 2.048 | 1.754 |
| amd64 Docker, final submission image with VM-local output | 1.673 | 1.523 |

The selected build keeps event payloads in input order and scatters 16-byte ID records into a single allocation per input chunk. Shards compare complete IDs for ambiguous records and return losing row positions. In five fresh-container runs per seed, this reduced median time by **12.3%** and **6.0%** relative to the previous executable in the same runtime. The 32-byte candidate reached 1.872 s on seed 17 and 2.488 s on seed 73 in that confirmation. Partitioned alias routing reached 2.345 s and 2.473 s. The root Dockerfile reproduced the selected scoring executable byte for byte.

One of the ten final native runs finished in **0.609 s**. The native medians exceeded 0.65 s by **0.150 s** and **0.195 s**. Individual native runs and official container scores use different clocks and execution environments.

The native comparison used five trials per seed with eight Rayon workers. The container confirmation used five trials per seed with VM-volume input and Mac bind-mounted output. The final VM-output diagnostic used three trials per seed. The native clock spans process launch to exit. The Docker clock uses the engine’s start and finish timestamps. Each trial wrote new Parquet tables. Input was read immediately before timing. The host remained available for normal desktop use, and trial times varied.

The Docker VM runs on Apple Silicon, translates linux/amd64 instructions and reports the x86-64-v3 feature level. Containers used eight pinned virtual CPUs, 7 GiB memory, no swap, no network, a read-only root and input mount, and uid 1000. The organizer supplies native x86-64 and 24 GB. Hidden practice and 60M final runs remain unmeasured.

## Changes tested

All values below are full-run medians in seconds, three trials per seed. Native controls are shown per series. Docker controls are shown for each container series. Candidate names describe changes relative to the original pipeline unless a row names a combination.

| Candidate | Native 17 | Native 73 | amd64 17 | amd64 73 |
| --- | ---: | ---: | ---: | ---: |
| First control | 1.285 | 1.091 | 2.808 | 2.211 |
| Cached calendar dates and word-level timestamps | 1.252 | 0.998 | 2.827 | 2.200 |
| Chunk-wide newline, comma and quote masks | 1.572 | 1.026 | 1.977 | 2.503 |
| Map for ambiguous IDs | 1.544 | 0.936 | 2.214 | 2.321 |
| Borrowed winner slots | 1.085 | 1.321 | 2.381 | 2.288 |
| 32-byte candidates with wide-window fallback | 1.855 | 0.933 | 2.087 | 2.153 |
| 16-byte ID metadata, separate event payload | 0.918 | 1.100 | 3.401 | 2.934 |
| Partitioned alias routing | 1.625 | 1.018 | 2.418 | 2.162 |
| Second native control¹ | 1.254 | 1.330 | 2.808 | 2.211 |
| Word-level timestamp arithmetic alone | 1.335 | 1.376 | 2.494 | 2.408 |
| 16-byte metadata with uniqueness filter, 256 shards | 0.941 | 0.892 | 3.171 | 2.983 |
| 16-byte metadata and word-level timestamps | 0.862 | 0.909 | 3.241 | 2.936 |
| Negative-membership filter for aliases | 1.246 | 1.322 | 2.076 | 2.384 |
| Third control | 1.226 | 1.246 | 2.673 | 2.424 |
| Filtered metadata with 64 shards | 1.011 | 0.802 | 2.272 | 2.697 |
| Chunk scanner and borrowed winner slots | 1.194 | 1.274 | 2.465 | 2.310 |
| Filtered metadata in contiguous slabs | 1.330 | 1.613 | 2.230 | 1.743 |
| Borrowed winner slots and alias filter | 1.427 | 1.745 | 3.012 | 2.315 |
| Alias filter, repeated control | 1.655 | 1.467 | 2.478 | 2.780 |

¹ The first two native series share one Docker comparison. The minimal runtime was also timed with the exact original binary. Its medians were 2.489 s and 2.232 s on seeds 17 and 73. All algorithm screens above used a common Debian runtime. The shortlist used the minimal runtime for both the original and the candidates.

## Correctness and remaining work

The final build passed all 36 official fixtures and the public seed-1 dev comparison on both platforms. The dev result contained 117,038 user/day rows and 27,400 campaign/day rows, with zero duplicate keys. Five additional small generated seeds matched an independent Python reference. On the two 20M seeds, all algorithm variants were compared row for row with the earlier scalar implementation. Seed 17 produced 2,249,563 user/day rows and 28,014 campaign/day rows. Seed 73 produced 2,230,619 and 28,014. The official answer covers the public 1M set.

The submission image is 7,683,286 bytes (7.68 MB). The original Debian image was 115,577,457 bytes. Both use the same pinned Debian runtime libraries. The image starts the scorer directly and includes the utilities used by the fixture and memory-check harnesses. The scoring binary uses the existing locked Cargo dependencies. A separate seed-17 run reported a cgroup peak of 1,227,743,232 bytes (1.14 GiB), 98,143,989 output bytes and zero scratch bytes. The amd64 Rust release build took 50.8 s with cached base images and registry downloads.

## Next experiments toward 0.65 s

1. **Parse clicks while the identity graph is being built.** The final amd64 run spent a median 1.017 s and 0.949 s on links, parsing and partitioning. Currently events overlap link construction, while click parsing waits for the graph. Parse click IDs, dates and campaigns into per-chunk buffers immediately and measure whether resolving the raw user IDs later pays for the extra pass. Validate click tie ordering and cross-device attribution on both seeds.
2. **Batch timestamp conversion across records.** The per-row timestamp microbenchmark improved from 9.83 ms to 6.07 ms, yet the cached-date version failed to deliver a consistent end-to-end gain. Use the existing CSV masks to batch fields of the same format within a chunk, then test AVX2 digit validation and conversion across records. Preserve the general calendar, arbitrary-width epoch and malformed-field paths. Time the full container to capture the cost of forming each batch.
3. **Measure input and output separately before tuning the writer.** Meeting 0.65 s requires consuming the supplied inputs at **2.88–2.96 GB/s** before accounting for output. With warmed inputs, the local amd64 shell-start control took 0.058 s and 0.081 s. A sequential control that also read every input file took 0.439 s and 0.855 s. The production reader uses parallel memory mapping, so those controls describe the local storage path. With VM-local output the final medians were 1.673 s and 1.523 s, around 0.38 s and 0.23 s below the bind-output confirmation. The two series were measured separately, so the differences include timing variation. The hidden scorer is the remaining way to establish the native amd64 competition time under the allowed local-machine scope.

## Earlier tuning round

The previous implementation used SIMD delimiter scans, lossless short-ID packing and mapped read-ahead. Its earlier native medians were 1.181 s and 1.173 s. On seed 17 the earlier amd64 median was 3.190 s with VM-volume input and 5.341 s with Mac bind-mounted input. In a separate seed-17 mapping experiment the medians were 2.933 s for plain mapping, 2.512 s with advisory read-ahead and 4.838 s for bulk reads. The current tables use contemporaneous controls for each comparison.
