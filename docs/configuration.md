# Configuration Reference

The helper reads `archive-vfs.toml` from Yazi's configuration directory. Set
`ARCHIVE_VFS_CONFIG` to use a different file. Every field is optional; unknown
fields are rejected so misspellings cannot silently disable a safety limit.

```toml
cache_dir = "/home/alice/.cache/archive-vfs/members"
max_cache_bytes = 2147483648
max_concurrent_extractions = 2
max_member_bytes = 8589934592
max_compression_ratio = 1000.0
index_dir = "/home/alice/.cache/archive-vfs/indexes"
persist_indexes = true
filename_policy = "standard"
log_level = "info"
archive_extensions = ["zip", "zipx"]
```

| Field | Default | Meaning |
| --- | ---: | --- |
| `cache_dir` | `$XDG_CACHE_HOME/archive-vfs/members` | Materialized member files and LRU metadata. |
| `max_cache_bytes` | 2 GiB | Target upper bound after inactive entries are evicted. Active leases may temporarily exceed it. |
| `max_concurrent_extractions` | 2 | Cross-process extraction slots. |
| `max_member_bytes` | 8 GiB | Maximum uncompressed size of one member. |
| `max_compression_ratio` | 1000 | Maximum declared uncompressed/compressed ratio. |
| `index_dir` | `$XDG_CACHE_HOME/archive-vfs/indexes` | Immutable SQLite central-directory indexes. |
| `persist_indexes` | `true` | Reuse disk indexes; `false` creates a fresh in-memory index per helper call. |
| `filename_policy` | `standard` | `standard`, `raw`, or `lossy-utf8`. |
| `log_level` | `info` | `off`, `error`, `warn`, `info`, `debug`, or `trace`. |
| `archive_extensions` | `zip`, `zipx` | Local extensions the enter-key adapter probes. Format probing still verifies content. |

`standard` honors a valid Unicode path extra field, then the UTF-8 language
flag, then ZIP's CP437 default. Invalid flagged UTF-8 is escaped. `raw` exposes
original bytes where the platform and Yazi URL model permit them.
