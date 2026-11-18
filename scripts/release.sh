#!/bin/sh
# Gera dist/recorta-<versão>-linux-x86_64.tar.gz compilando no Ubuntu 20.04
# (glibc 2.31), para o binário rodar em distros antigas e novas.
# Requer podman. Cache de build em ~/.cache/recorta-build.
set -eu
cd "$(dirname "$0")/.."
V=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
N="recorta-$V-linux-x86_64"
C="$HOME/.cache/recorta-build"
mkdir -p "$C/target" "$C/cargo" dist

podman run --rm -v "$PWD":/src:ro,Z -v "$C/target":/target:Z -v "$C/cargo":/cargo:Z \
    -e CARGO_HOME=/cargo -e CARGO_TARGET_DIR=/target -e RUSTUP_HOME=/cargo/rustup \
    -e DEBIAN_FRONTEND=noninteractive docker.io/library/ubuntu:20.04 sh -c '
    set -e
    apt-get update -qq && apt-get install -y -qq gcc libc6-dev curl ca-certificates >/dev/null
    [ -x /cargo/bin/cargo ] || curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --no-modify-path
    /cargo/bin/rustup update -q stable >/dev/null 2>&1 || true
    /cargo/bin/cargo build --release --locked -j10 --manifest-path /src/Cargo.toml'

rm -rf "dist/${N:?}" "dist/$N.tar.gz" "dist/$N.tar.gz.sha256"
mkdir -p "dist/$N"
cp "$C/target/release/recorta" instalar.sh README.md LICENSE assets/recorta.svg assets/recorta.desktop "dist/$N/"
(cd dist && tar czf "$N.tar.gz" "$N" && sha256sum "$N.tar.gz" > "$N.tar.gz.sha256")
echo "glibc máxima exigida: $(objdump -T "dist/$N/recorta" | grep -oE 'GLIBC_[0-9.]+' | sort -Vu | tail -1)"
cat "dist/$N.tar.gz.sha256"
