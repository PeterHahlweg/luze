RUST_TOOLCHAIN_DEFAULT ?= stable

.PHONY: coverage
coverage: coverage-setup
	cargo llvm-cov

.PHONY: coverage-report
coverage-report: coverage-setup
	cargo llvm-cov --html
	open target/llvm-cov/html/index.html

.PHONY: coverage-setup
coverage-setup: cargo-setup
	cargo install cargo-llvm-cov
	rustup component add llvm-tools-preview --toolchain $(RUST_TOOLCHAIN_DEFAULT)

.PHONY: loc
loc: loc-setup
	tokei .

.PHONY: loc-setup
loc-setup: rustup
	cargo install tokei

.PHONY: cargo-setup
cargo-setup: rustup
	rustup default $(RUST_TOOLCHAIN_DEFAULT)

.PHONY: rustup
rustup:
	@if command -v rustup >/dev/null 2>&1; then echo "rustup installed"; else curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh; fi
