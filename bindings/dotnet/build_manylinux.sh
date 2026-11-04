#!/bin/bash
# Run from the repository root inside quay.io/pypa/manylinux_2_28_x86_64
# as the invoking uid:gid. Both the C ABI library and worker use glibc 2.28.
set -euo pipefail
export HOME=/tmp/home CARGO_HOME=/tmp/cargo RUSTUP_HOME=/tmp/rustup CARGO_TARGET_DIR=/tmp/target
export PATH="$CARGO_HOME/bin:$PATH"
mkdir -p "$HOME"
toolchain=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$toolchain"
rustc -vV
/opt/python/cp312-cp312/bin/python bindings/dotnet/stage_native.py --rid linux-x64
