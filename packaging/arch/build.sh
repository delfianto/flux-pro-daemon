#!/usr/bin/env bash
set -euo pipefail

recipe=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repo=$(cd -- "$recipe/../.." && pwd -P)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT

export PKGDEST
PKGDEST=$(realpath -m -- "${PKGDEST:-$repo/dist}")
mkdir -p -- "$PKGDEST"

archive=fluxpro-source.tar.gz
tar -czf "$work/$archive" --exclude=packaging/arch -C "$repo" \
    Cargo.toml Cargo.lock rust-toolchain.toml LICENSE src tests packaging

cd -- "$work"
checksum=$(sha256sum "$archive")
sed "s/^sha256sums=.*/sha256sums=('${checksum%% *}')/" "$recipe/PKGBUILD" > PKGBUILD
cp -- "$recipe/fluxpro.install" .
makepkg "$@"
