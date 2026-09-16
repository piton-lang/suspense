#!/usr/bin/env bash
# Names a release build: the version from Cargo.toml, today's date (UTC), and
# which build of the day this is, counting the releases already tagged today.
#
#   Suspense 0.1.0 (2026-09-17.2)
#
# Prints, one per line, for $GITHUB_OUTPUT:
#   version=0.1.0
#   build=2026-09-17.2                         shown in the name
#   bundle_version=20260917.2                  macOS's CFBundleVersion: numbers and dots
#   tag=v0.1.0-2026-09-17.2
#   title=Suspense 0.1.0 (2026-09-17.2)
#   file=Suspense-0.1.0-2026-09-17.2           the start of each zip's name
#
# Tags are read with `git ls-remote --tags`, from $TAGS_FROM (default origin).
# $TODAY overrides the date, for testing.
set -euo pipefail

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
today=${TODAY:-$(date -u +%Y-%m-%d)}

# The highest sequence already tagged for this version today.
last=$(git ls-remote --tags "${TAGS_FROM:-origin}" "v${version}-${today}.*" \
  | sed -n "s|.*refs/tags/v${version}-${today}\.\([0-9][0-9]*\)$|\1|p" \
  | sort -n | tail -1)
sequence=$(( ${last:-0} + 1 ))

build="${today}.${sequence}"
echo "version=${version}"
echo "build=${build}"
echo "bundle_version=${today//-/}.${sequence}"
echo "tag=v${version}-${build}"
echo "title=Suspense ${version} (${build})"
echo "file=Suspense-${version}-${build}"
