# Security Model

Archive contents and metadata are untrusted. `archive-vfs` never constructs a
cache path from a member name: cache entries are keyed by archive identity and
central-directory entry ID. Parent components are rendered as safe virtual
names and cannot traverse the cache or local filesystem.

Extraction enforces configurable uncompressed-size and compression-ratio
limits before reading data, streams through a 128 KiB buffer, and verifies the
uncompressed size and CRC before atomic publication. Encrypted entries,
multi-disk archives, unsupported compression methods, inconsistent bounds, and
corrupt headers fail with explicit errors.

The cache uses cross-process locks. One writer materializes a member into a
partial file; readers receive a shared lease only after rename. Eviction skips
locked files and runs after materialization and lease release. Abandoned
partials can be removed with `archive-vfs-helper cache-clean`.

The helper never invokes a shell. Lua supplies each archive path and virtual
member path as a separate process argument, including paths with spaces,
quotes, brackets, and shell metacharacters.

The provider is intentionally read-only. Create, write, truncate, rename,
remove, directory creation, hard-link, symlink, and trash operations return a
`PermissionDenied` filesystem error, and capabilities advertise no mutation.
