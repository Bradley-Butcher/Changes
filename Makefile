.PHONY: check fmt lint test build release clean

# Run all checks (what CI runs)
check: fmt lint test build

fmt:
	cargo fmt --check

lint:
	cargo clippy --all-targets

test:
	cargo test

build:
	cargo build

release:
	cargo build --release

clean:
	cargo clean

# Format in-place
fix:
	cargo fmt
	cargo clippy --fix --allow-dirty

# Install locally
install:
	cargo install --path .
