.PHONY: check fmt fmt-fix clippy test build clean

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
