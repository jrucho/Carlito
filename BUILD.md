# Build Carlito from source

The downloadable release is already compiled. These steps are for developers.

## Tests (macOS or Linux)

```sh
cargo test --manifest-path app/Cargo.toml
```

## Paper Pro Move takeover build (Linux SDK)

Install Rust and the matching reMarkable chiappa SDK on Linux. For the original
Move OS 3.26 installation the SDK is 3.26.0.68 / device image 5.6.75.
Use an SDK matching your tablet's OS; other versions require device testing.

```sh
rustup target add aarch64-unknown-linux-gnu
export RM_SDK=/path/to/installed/chiappa-sdk
mkdir -p quill/vendor
scp -O root@10.11.99.1:/usr/lib/plugins/scenegraph/libqsgepaper.so quill/vendor/
./scripts/build-move.sh
```

If the SDK includes that vendor library, the quill build can use it directly.
Never commit or redistribute the proprietary vendor library.

Output: `dist/carlito/`. Packaging copies configuration templates only, not a
real key. Zip that directory to produce an installable release.

An alternative macOS/Linux build can use Zig and cargo-zigbuild with an existing
matching Move `libquill.so` in `quill/build/` and vendor library in `quill/vendor/`:

```sh
cargo zigbuild --manifest-path app/Cargo.toml --release --target aarch64-unknown-linux-gnu --features takeover
python3 scripts/normalize-quill-needed.py app/target/aarch64-unknown-linux-gnu/release/carlito
./scripts/make-bundle.sh
```

The CI workflow checks the source on Linux. Device display-library builds use
your device/SDK library and are performed separately for release bundles.
