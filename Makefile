# Developer aliases. The binary installs itself; there is nothing else to do.
.PHONY: install status dev test-dev ci

install:
	cargo build --release && target/release/br8n install

status:
	@target/release/br8n status

dev:
	cargo build --no-default-features

test-dev:
	cargo test --no-default-features

ci:
	scripts/ci-local.sh
