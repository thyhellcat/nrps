# Building nrps

nrps is written in Rust. The server is built as a static Linux binary for the
`x86_64-unknown-linux-musl` target, either on Linux or on Windows.

## Building on Linux

Prerequisites: Rust installed with rustup, a clone of this repository (`git clone`),
and `musl-tools` (`sudo apt install musl-tools` on Debian or Ubuntu). Then, in the
repository folder:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

## Building on Windows

Prerequisites: Rust installed with rustup, LLVM installed in `C:\Program Files\LLVM`,
Git Bash, and a clone of this repository. Run these commands in Git Bash, in the
repository folder. The `RUSTFLAGS` line keeps your Windows user name and home folder
out of the binary:

```bash
rustup target add x86_64-unknown-linux-musl
export CC_x86_64_unknown_linux_musl="C:/Program Files/LLVM/bin/clang.exe"
export AR_x86_64_unknown_linux_musl="C:/Program Files/LLVM/bin/llvm-ar.exe"
export CFLAGS_x86_64_unknown_linux_musl="--target=x86_64-unknown-linux-musl -DNDEBUG -DRING_CORE_NOSTDLIBINC=1 -ffreestanding"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld
export RUSTFLAGS="--remap-path-prefix=$(cygpath -m "${CARGO_HOME:-$USERPROFILE/.cargo}")/registry/src=/cargo/registry/src"
cargo build --release --target x86_64-unknown-linux-musl
```

## Output

On both platforms the binary is `target/x86_64-unknown-linux-musl/release/nrps`. To
install it on a server, see [deploying nrps](deployment.md).

## Tests and lints

These are the checks that CI runs:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
