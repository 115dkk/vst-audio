#!/bin/bash
# Builds the Linux wheel inside quay.io/pypa/manylinux_2_28_x86_64, so the
# extension and the bundled worker link the same old glibc (ADR 0006). Run from
# the repository root as the invoking user; everything except the staged worker
# and dist/python stays under /tmp in the container.
set -euo pipefail
export HOME=/tmp/home CARGO_HOME=/tmp/cargo RUSTUP_HOME=/tmp/rustup CARGO_TARGET_DIR=/tmp/target
export PATH="$CARGO_HOME/bin:$PATH"
mkdir -p "$HOME"
toolchain=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' rust-toolchain.toml)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
  | sh -s -- -y --no-modify-path --profile minimal --default-toolchain "$toolchain"
rustc -vV
/opt/python/cp312-cp312/bin/python -m venv /tmp/venv
. /tmp/venv/bin/activate
pip install --quiet maturin==1.16.0 auditwheel==6.8.2
python bindings/python/stage_worker.py
cd bindings/python
maturin build --locked --release --compatibility manylinux_2_28 --out ../../dist/python
auditwheel show ../../dist/python/*.whl
