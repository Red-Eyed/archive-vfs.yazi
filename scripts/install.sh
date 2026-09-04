#!/bin/sh
set -eu

minimum_yazi=26.9.1
bin_dir=${ARCHIVE_VFS_BIN_DIR:-"${HOME}/.local/bin"}
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(dirname "$script_dir")
cd "$repository_root"

if [ "$#" -ne 0 ]; then
	echo "usage: ARCHIVE_VFS_BIN_DIR=DIR scripts/install.sh" >&2
	exit 2
fi

if ! command -v cargo >/dev/null 2>&1; then
	echo "archive-vfs: cargo is required to build the helper" >&2
	exit 1
fi

if ! command -v yazi >/dev/null 2>&1; then
	echo "archive-vfs: Yazi $minimum_yazi or newer is required" >&2
	exit 1
fi

yazi_version=$(yazi --version | awk '
	match($0, /[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*/) {
		print substr($0, RSTART, RLENGTH)
		exit
	}
')
if [ -z "$yazi_version" ]; then
	echo "archive-vfs: could not determine the installed Yazi version" >&2
	exit 1
fi

if ! awk -v have="$yazi_version" -v need="$minimum_yazi" 'BEGIN {
	split(have, h, "."); split(need, n, ".");
	for (i = 1; i <= 3; i++) {
		if ((h[i] + 0) > (n[i] + 0)) exit 0;
		if ((h[i] + 0) < (n[i] + 0)) exit 1;
	}
	exit 0;
}'; then
	echo "archive-vfs: Yazi $yazi_version is unsupported; install $minimum_yazi or newer" >&2
	exit 1
fi

case "$bin_dir" in
	/usr|/usr/*|/bin|/bin/*|/sbin|/sbin/*)
		echo "archive-vfs: refusing system install directory: $bin_dir" >&2
		exit 1
		;;
esac

cargo build --locked --release --bin archive-vfs-helper
install -d "$bin_dir"
install -m 755 target/release/archive-vfs-helper "$bin_dir/archive-vfs-helper"

echo "Installed archive-vfs-helper in $bin_dir"
echo "The plugin configuration snippets are in README.md; this script did not edit Yazi configuration."
