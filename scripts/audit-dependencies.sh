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

cargo audit --ignore RUSTSEC-2023-0071
