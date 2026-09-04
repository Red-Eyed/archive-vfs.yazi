# Benchmarks

Measured on 2026-09-04 on an Apple Silicon development machine with the release
profile. Fixtures used stored zero-byte metadata entries and 100 neighboring
64 KiB deflated pseudo-image members. The sparse archive had a logical size of
100 GiB and one member; it consumed negligible physical storage. Run
`scripts/benchmark.sh`, or set `ARCHIVE_VFS_BENCH_ENTRIES=1000000` for the
larger case.

| Measurement | 100k entries | 1m entries |
| --- | ---: | ---: |
| Initial central-directory index | 703 ms | 6.99 s |
| Cached full root listing | 10.4 ms | 109.9 ms |
| Resident memory with full result | 16.4 MiB | 92.0 MiB |
| First image materialization | 9.2 ms | 10.0 ms |
| Repeated same image | 1.69 ms | 1.68 ms |
| Sequential first access, 100 images | 782 ms | 737 ms |
| Random first access, 100 images | 854 ms | 784 ms |
| Index/list sparse logical 100 GiB ZIP64 | 0.58 ms | 0.81 ms |

The two runs were separate processes and are not a statistical comparison of
image timings. They show the intended scaling distinction: initial indexing
and a flat listing scale with entry count, while member access and indexing a
huge sparse archive with a tiny central directory do not scale with total
archive bytes.

Yazi 26.9.1 requires `ReadDir` to return all direct children in one Lua table.
The Rust resident numbers above do not include Yazi/Lua's additional table and
userdata allocation. A million direct children therefore works but cannot be
described honestly as instant or low-memory.
