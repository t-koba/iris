#!/usr/bin/env bash
set -euo pipefail

# RUSTSEC-2023-0071 is attached to russh's optional RSA implementation. Iris
# deliberately keeps that feature out of the default dependency graph; fail if
# it ever becomes reachable before applying the narrowly scoped audit ignore.
if cargo tree --workspace --locked --offline -e features -i rsa >/dev/null 2>&1; then
  echo "the vulnerable rsa crate is enabled in the default workspace graph" >&2
  cargo tree --workspace --locked --offline -e features -i rsa >&2
  exit 1
fi

# Isolated workspaces (fuzz targets and the Windows prototype) have their own
# lockfiles; audit every tracked lock so a stale pin cannot hide there. The
# narrow rsa ignore above applies only to the default workspace graph.
mapfile -t lockfiles < <(git ls-files '*Cargo.lock')
if ((${#lockfiles[@]} == 0)); then
  echo "no tracked Cargo.lock files found" >&2
  exit 1
fi

for lock in "${lockfiles[@]}"; do
  echo "auditing $lock"
  if [[ "$lock" == "Cargo.lock" ]]; then
    cargo audit --file "$lock" --ignore RUSTSEC-2023-0071
  else
    cargo audit --file "$lock"
  fi
done
