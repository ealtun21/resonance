set shell := ["bash", "-c"]

default: check

check: fmt clippy test

fmt:
    cargo fmt --all -- --check

fmt-fix:
    cargo fmt --all

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

test:
    cargo test --all

build:
    cargo build --all

clean:
    cargo clean
