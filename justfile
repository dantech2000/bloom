# Jellyui task runner. Run `just` to list recipes.

set shell := ["sh", "-cu"]

# Set MPV_LIB_DIR in the environment if libmpv is not found automatically.
export RUST_LOG := env_var_or_default("RUST_LOG", "jellyui=info")

default:
    @just --list

# Build a debug binary
build:
    cargo build

# Build an optimized binary
release:
    cargo build --release

# Run the app (debug build, logs to stderr)
run *ARGS:
    cargo run -- {{ARGS}}

# Run the optimized build
run-release:
    cargo run --release

# Run all tests, including the live libmpv playback round-trip (needs ffmpeg)
test *ARGS:
    cargo test --bin jellyui {{ARGS}}

# Lint with warnings treated as errors
lint:
    cargo clippy --bin jellyui --all-targets -- -D warnings

# Format the source tree
fmt:
    cargo fmt

# Verify formatting, lints and tests, as CI would
check: fmt-check lint test

# Fail if the tree is not formatted
fmt-check:
    cargo fmt --check

# Update installed gpuicn components to the pinned registry (keeps local edits unless --overwrite)
ui-update *ARGS:
    gpuicn add avatar button dialog input menu progress scroll-area select separator sidebar slider tabs toast tooltip {{ARGS}}

# Show which gpuicn component files would be overwritten by an update
ui-diff:
    gpuicn add avatar button dialog input menu progress scroll-area select separator sidebar slider tabs toast tooltip --dry-run --overwrite

# Install the gpuicn CLI at the pinned release
ui-cli:
    cargo install gpuicn-cli --git https://github.com/devaryakjha/gpuicn --tag v0.5.0-beta.3 --locked

# Print where the build script found libmpv, or how to point at it
mpv-info:
    @cargo build 2>&1 | grep -i "libmpv" || echo "libmpv located automatically (see build.rs for the search order)"
    @command -v mpv >/dev/null && mpv --version | head -1 || echo "mpv binary not on PATH"

# Remove the app's saved servers, profiles and tokens (asks first)
reset-config:
    @printf 'Delete %s? [y/N] ' "$HOME/Library/Application Support/jellyui/config.json"; read ans; [ "$ans" = y ] && rm -f "$HOME/Library/Application Support/jellyui/config.json" && echo removed || echo kept

# Remove build artifacts
clean:
    cargo clean
