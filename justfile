doc:
    cargo doc --open

doc-build:
    cargo doc

test:
    cargo test

lint-check:
    cargo clippy -- -D warnings
    cargo-sort --workspace --check
    cargo fmt --check

lint:
    cargo-sort --workspace
    cargo fmt
    cargo clippy -- -D warnings

build:
    cargo build --release

bench:
    cargo test bench -- --nocapture

clean:
    cargo clean
