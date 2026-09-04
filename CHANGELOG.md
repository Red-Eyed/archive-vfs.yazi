# Changelog

## [Unreleased]

### Bug Fixes

- Report missing helper executables and rejected ZIP archives instead of
  silently ignoring enter actions, and log plugin entry and VFS operations for
  runtime diagnosis.
- Honor `CARGO_TARGET_DIR` when installing the helper so isolated builds and
  non-default Cargo target directories remain discoverable.

## [0.1.1] - 2026-09-04

### Fixed

- Detect the installed Yazi version from both compact and labeled
  `yazi --version` output.

### Changed

- Make the single-line `curl` command the primary installation path and use a
  Git checkout as the alternative, without requiring `ya`.

## [0.1.0] - 2026-09-04

### Highlights

- Add a read-only Yazi virtual filesystem architecture with ZIP and ZIP64 as
  the first archive backend.
