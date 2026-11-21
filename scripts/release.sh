#!/usr/bin/env bash
# Bump the version, update the changelog, commit and tag.
# Usage: scripts/release.sh 0.2.0 [--push]
set -euo pipefail

cd "$(dirname "$0")/.."
new="${1:-}"
push="${2:-}"
if ! [[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "usage: $0 X.Y.Z [--push]" >&2
    exit 1
fi
if [ -n "$(git status --porcelain)" ]; then
    echo "working tree is not clean" >&2
    exit 1
fi
if git rev-parse "v$new" >/dev/null 2>&1; then
    echo "tag v$new already exists" >&2
    exit 1
fi

sed -i "0,/^version = \".*\"/s//version = \"$new\"/" Cargo.toml
sed -i "s/^pkgver=.*/pkgver=$new/; s/^pkgrel=.*/pkgrel=1/" packaging/arch/PKGBUILD
cargo update --workspace --quiet

today=$(date +%Y-%m-%d)
if grep -q "^## \[Unreleased\]" CHANGELOG.md; then
    sed -i "s/^## \[Unreleased\]/## [Unreleased]\n\n## [$new] - $today/" CHANGELOG.md
else
    sed -i "0,/^## /s//## [$new] - $today\n\n## /" CHANGELOG.md
fi

cargo test --quiet --locked
git add Cargo.toml Cargo.lock packaging/arch/PKGBUILD CHANGELOG.md
git commit -m "Release v$new"
git tag -a "v$new" -m "v$new"
echo "Tagged v$new."
if [ "$push" = "--push" ]; then
    git push origin HEAD "v$new"
    echo "Pushed; the Release workflow builds and publishes the packages."
else
    echo "Run: git push origin HEAD v$new"
fi
