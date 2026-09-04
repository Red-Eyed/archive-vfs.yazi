#!/bin/sh
set -eu

repository_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
work_root=$repository_root/work_dir
mkdir -p "$work_root"
test_root=$(mktemp -d "$work_root/install.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT HUP INT TERM

fake_bin=$test_root/fake-bin
install_bin=$test_root/install-bin
build_dir=$test_root/target
mkdir -p "$fake_bin"

cat >"$fake_bin/yazi" <<'EOF'
#!/bin/sh
printf '%s\n' 'Yazi 26.9.1 (integration test)'
EOF

cat >"$fake_bin/cargo" <<'EOF'
#!/bin/sh
set -eu
test "$*" = "build --locked --release --bin archive-vfs-helper"
mkdir -p "$CARGO_TARGET_DIR/release"
cat >"$CARGO_TARGET_DIR/release/archive-vfs-helper" <<'HELPER'
#!/bin/sh
printf '%s\n' 'archive-vfs-helper integration-test'
HELPER
chmod 755 "$CARGO_TARGET_DIR/release/archive-vfs-helper"
EOF

chmod 755 "$fake_bin/yazi" "$fake_bin/cargo"

PATH="$fake_bin:/usr/bin:/bin" \
	CARGO_TARGET_DIR="$build_dir" \
	ARCHIVE_VFS_BIN_DIR="$install_bin" \
	"$repository_root/scripts/install.sh" >/dev/null

installed=$(PATH="$install_bin:$fake_bin:/usr/bin:/bin" command -v archive-vfs-helper)
test "$installed" = "$install_bin/archive-vfs-helper"
test "$("$installed" --version)" = "archive-vfs-helper integration-test"

printf '%s\n' "installer discovery integration test passed"
