.PHONY: build install test
build:            ## ./devsweep
	cargo build --release && cp target/release/devsweep .
install:          ## into ~/.cargo/bin
	cargo install --path .
test:
	cargo clippy --all-targets -- -D warnings && cargo test
