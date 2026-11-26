# Shortcuts for the commands in CONTRIBUTING.md. CI in `.github/workflows/ci.yml` stays the gate.
# Run `make` or `make help` to list the targets.

.DEFAULT_GOAL := help
.PHONY: help build run test test-one test-ncdu snapshots fmt fmt-check clippy typos shear deny \
	man check bench tools hooks doc clean

# Arguments for `make run`, e.g. `make run ARGS="-o- ."`.
ARGS ?=
# Test name filter for `make test-one`, e.g. `make test-one T=exclude`.
T ?=

# Keep the prettier version in sync with the fmt job in CI and the prettier hook in `prek.toml`.
prettier := npx prettier@3.9.0

# Tool versions come from CI, where Renovate keeps them up to date. A key that is missing or that
# matches more than once stops make instead of passing a bad version to `cargo install`.
ci_version = $(call ci_version_once,$(1),$(shell sed -n 's/^  $(1): \([0-9][0-9A-Za-z.+-]*\)$$/\1/p' .github/workflows/ci.yml))
ci_version_once = $(if $(filter 1,$(words $(2))),$(2),$(error $(1) not found exactly once in .github/workflows/ci.yml))

help: ## List the targets
	@grep -E '^[a-z-]+:.*## ' $(MAKEFILE_LIST) | awk -F ':.*## ' '{printf "  %-11s %s\n", $$1, $$2}'

build: ## Build every crate with the dev profile
	cargo build --workspace

run: ## Run the duit binary, passing ARGS
	cargo run --bin duit -- $(ARGS)

test: ## Run all tests
	cargo nextest run --workspace

test-one: ## Run the tests whose name matches T
	$(if $(T),,$(error Set T, e.g. make test-one T=exclude))
	cargo nextest run --workspace -E 'test($(T))'

test-ncdu: ## Run all tests, including those comparing with ncdu 2.9.2 on PATH
	cargo nextest run --workspace --features test-ncdu

snapshots: ## Run all tests and accept snapshot changes (needs cargo-insta)
	cargo insta test --accept --test-runner nextest

fmt: ## Format Rust, TOML, Markdown, YAML, and JSON files
	cargo fmt --all
	taplo fmt
	$(prettier) --write .

fmt-check: ## Check formatting (fixes TOML, Markdown, YAML, and JSON files in place)
	cargo fmt --all --check
	@# Fix the files, then fail so the change gets noticed.
	taplo fmt --check || { taplo fmt && false; }
	$(prettier) --check . || { $(prettier) --write . && false; }

clippy: ## Lint with clippy, warnings are errors
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

typos: ## Check spelling
	typos

shear: ## Check for unused dependencies
	cargo shear --deny-warnings

deny: ## Check licenses, sources, and bans of dependencies
	cargo deny --all-features check licenses sources bans
	cargo deny --manifest-path fuzz/Cargo.toml --all-features check licenses sources bans

man: ## Lint the man page with mandoc
	mandoc -T lint -W warning crates/duit/duit.1

check: fmt-check clippy typos shear deny man ## Run the CI checks except zizmor and the publish dry run, then the tests the way CI does
	cargo nextest run --workspace --locked --profile ci --cargo-profile fast-build

bench: ## Compare scan time and peak memory with ncdu 2.9.2, see BENCHMARKS.md
	python3 scripts/benchmark.py

tools: ## Install the tools that CI uses, at the versions that CI pins
	cargo install --locked cargo-nextest@$(call ci_version,CARGO_NEXTEST_VERSION)
	cargo install --locked cargo-deny@$(call ci_version,CARGO_DENY_VERSION)
	cargo install --locked cargo-shear@$(call ci_version,CARGO_SHEAR_VERSION)
	cargo install --locked taplo-cli@$(call ci_version,TAPLO_VERSION)
	cargo install --locked typos-cli@$(call ci_version,TYPOS_VERSION)

hooks: ## Install the prek git hooks
	prek install

doc: ## Build the API docs and open them in a browser
	cargo doc --workspace --no-deps --open

clean: ## Delete the target directory
	cargo clean
