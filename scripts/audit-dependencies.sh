#!/usr/bin/env bash
set -euo pipefail

# RUSTSEC-2023-0071 is attached to russh's optional RSA implementation. Iris
# deliberately keeps that feature out of the default dependency graph; fail if
# it ever becomes reachable before applying the narrowly scoped audit ignore.
if tree_output=$(cargo tree --workspace --locked --offline -e features -i rsa 2>&1); then
  echo "the vulnerable rsa crate is enabled in the default workspace graph" >&2
  printf '%s\n' "$tree_output" >&2
  exit 1
fi

# Cargo reports an absent package with an exact first error line followed by
# help text. Any other result -- including a second error or a resolution
# diagnostic -- means the query itself failed and must not become an ignore.
absence_error='error: package ID specification `rsa` did not match any packages'
unexpected_tree_result() {
  echo "cargo tree could not confirm that rsa is absent; refusing to ignore RUSTSEC-2023-0071" >&2
  printf '%s\n' "$tree_output" >&2
  exit 1
}
if [[ "${tree_output%%$'\n'*}" != "$absence_error" ]]; then
  unexpected_tree_result
fi
if ! printf '%s\n' "$tree_output" | awk '
    NR > 1 && $0 != "" && $0 !~ /^[[:space:]]*help:/ { extra = 1 }
    END { exit extra ? 1 : 0 }
  '; then
  unexpected_tree_result
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
