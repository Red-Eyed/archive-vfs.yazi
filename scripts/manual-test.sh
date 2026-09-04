#!/bin/sh
set -eu

script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(dirname "$script_dir")
cd "$repository_root"

if [ "$#" -ne 0 ]; then
	echo "usage: scripts/manual-test.sh" >&2
	exit 2
fi

if ! command -v yazi >/dev/null 2>&1; then
	echo "manual test requires Yazi 26.9.1 or newer on PATH" >&2
	exit 1
fi

test_root=$(mktemp -d "${TMPDIR:-/tmp}/archive-vfs-manual.XXXXXX")
trap 'if [ -n "$test_root" ]; then rm -rf -- "$test_root"; fi' EXIT HUP INT TERM

config_root=$test_root/config
plugin_root=$config_root/plugins/archive-vfs.yazi
mkdir -p "$config_root/plugins"
ln -s "$repository_root" "$plugin_root"
cp tests/manual/vfs.toml "$config_root/vfs.toml"
cp tests/manual/yazi.toml "$config_root/yazi.toml"
cp tests/manual/keymap.toml "$config_root/keymap.toml"

helper_config=$test_root/archive-vfs.toml
printf 'cache_dir = "%s"\nindex_dir = "%s"\nlog_level = "debug"\n' \
	"$test_root/cache/members" "$test_root/cache/indexes" >"$helper_config"

cargo build --locked --release --bin archive-vfs-helper
cargo run --locked --quiet --example integration_fixture -- "$test_root/dataset.zip"

echo "Verify image, JSON, nested text/source previews, copy, and leave navigation."
YAZI_CONFIG_HOME="$config_root" \
	ARCHIVE_VFS_CONFIG="$helper_config" \
	ARCHIVE_VFS_HELPER="$repository_root/target/release/archive-vfs-helper" \
	yazi "$test_root"
