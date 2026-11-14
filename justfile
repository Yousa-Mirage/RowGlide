alias check := lint
alias doc := document

# List available development commands.
default:
    @just --list

# Run the application in dev mode.
dev:
    cargo run -p rowglide-app --bin rowglide

# Run the application in release mode.
release:
    cargo run -p rowglide-app --bin rowglide --release

# Run all tests.
test:
    cargo test --all

# Format and clippy.
lint:
    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings

# Build the local macOS development bundle.
bundle-macos:
    sh scripts/bundle-macos.sh

# Document the codebase.
document:
    cargo doc --workspace --no-deps
