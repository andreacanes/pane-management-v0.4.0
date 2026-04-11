#!/usr/bin/env bash
# Sync this WSL source-of-truth to the Windows build scratch.
# The scratch keeps persistent build artifacts (target/, node_modules/,
# .gradle/) so cargo/gradle get fast incremental builds.
#
# - Run this before any cmd.exe build command.
# - Never commit from the Windows scratch — its .git dirs have been
#   removed on purpose. All commits/pushes happen from this WSL dir.
# - The scratch is disposable: `rm -rf <SCRATCH>` and the next sync
#   rebuilds a source tree. First rebuild after a clean will be cold.

set -euo pipefail

SRC="/home/andrea/pane-management/"
DEST="/mnt/c/Users/Andrea/Desktop/Botting/pane-management-v0.4.0/"

if [[ ! -d "$DEST" ]]; then
  echo "Creating scratch dir: $DEST"
  mkdir -p "$DEST"
fi

exec rsync -a --delete \
  --exclude '.git' \
  --exclude '.git/' \
  --exclude 'target/' \
  --exclude 'node_modules/' \
  --exclude 'dist/' \
  --exclude 'build/' \
  --exclude 'app/build/' \
  --exclude '.gradle/' \
  --exclude '.kotlin/' \
  --exclude 'local.properties' \
  --exclude 'sync.sh' \
  "$SRC" "$DEST"
