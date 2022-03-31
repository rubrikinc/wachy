#!/bin/bash -ex
# Basic script to generate release artifacts under target/publish.

cargo build --release
cargo deb

rm -r target/publish
mkdir -p target/publish
cp target/release/wachy target/publish/
cp target/debian/*.deb target/publish/

cd target/publish
NAME=$(basename *.deb .deb)
tar czvf $NAME.tar.gz wachy
rm wachy
