# List available development commands.
default:
    @just --list

# Run the native application with the mock backend.
dev:
    cargo run -p rowglide-ui --bin rowglide

# Run workspace tests, including documentation tests.
test:
    cargo test --workspace

# Treat every Clippy warning as an error, including in tests.
lint:
    cargo fmt --all
    cargo clippy --workspace --all-targets -- -D warnings

# Build the local macOS development bundle (no signing or installation).
bundle-macos:
    sh scripts/bundle-macos.sh
