#!/usr/bin/env bash
# On a tag build, stamp the tag's version (v0.2 -> 0.2.0, v1.0.0-beta.1 -> 1.0.0-beta.1)
# into Cargo.toml and Cargo.lock so every package carries the released version.
set -euo pipefail
[[ "${GITHUB_REF:-}" == refs/tags/v* ]] || exit 0

v="${GITHUB_REF_NAME#v}"
core="${v%%[-+]*}"
rest="${v:${#core}}"
case "$core" in
  *.*.*) ;;
  *.*) core="$core.0" ;;
  *) core="$core.0.0" ;;
esac
v="$core$rest"
[[ "$core" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "::error::Tag $GITHUB_REF_NAME is not a version"; exit 1; }

V="$v" perl -0pi -e 's/^version = "[^"]*"/version = "$ENV{V}"/m' Cargo.toml
V="$v" perl -0pi -e 's/(name = "pdfmonster"\r?\nversion = ")[^"]*/$1$ENV{V}/' Cargo.lock
grep -q "^version = \"$v\"" Cargo.toml
echo "Building version $v"
