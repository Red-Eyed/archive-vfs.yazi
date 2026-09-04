#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(dirname "$script_dir")
cd "$repository_root"

entry_count=${ARCHIVE_VFS_BENCH_ENTRIES:-100000}
if [ "$#" -gt 1 ]; then
	echo "usage: scripts/benchmark.sh [workspace]" >&2
	exit 2
fi

benchmark_temp=""
if [ "$#" -eq 1 ]; then
	benchmark_root=$1
	mkdir -p "$benchmark_root"
else
	benchmark_temp=$(mktemp -d "${TMPDIR:-/tmp}/archive-vfs-benchmark.XXXXXX")
	benchmark_root=$benchmark_temp
	trap 'if [ -n "$benchmark_temp" ]; then rm -rf -- "$benchmark_temp"; fi' EXIT HUP INT TERM
fi

benchmark_root=$(mkdir -p "$benchmark_root" && CDPATH='' cd -- "$benchmark_root" && pwd)

echo "Generating $entry_count-entry fixture in $benchmark_root" >&2
cargo run --release --example benchmark -- generate "$benchmark_root" "$entry_count"

/usr/bin/time -p cargo run --release --example benchmark -- measure "$benchmark_root"
