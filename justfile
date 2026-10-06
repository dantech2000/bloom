# Bloom task runner. Run `just` to list recipes. The scripts are in dev/.

set shell := ["sh", "-cu"]

default:
    @just --list

# Build the optimized binary
build:
    cargo build --release

# Start (or restart) the app from a light app bundle, with the debug channel on
run:
    dev/run

# Run all tests (the ones with the real player need ffmpeg)
test *ARGS:
    dev/test {{ARGS}}

# Walk every page in a test instance and check the frame cost
smoke:
    dev/smoke

# Build target/Bloom.app with libmpv inside
bundle:
    dev/bundle

# Build the app and copy it to /Applications
install:
    dev/install

# Make a signed release in target/dist: `just release 0.2.0`; `just release key` prints the public key
release *ARGS:
    dev/release {{ARGS}}

# Build libmpv from pinned sources into vendor/mpv (needed once before the first build)
build-mpv:
    dev/build-mpv

# Send a command to the running app, for example: just ctl state
ctl *ARGS:
    dev/jctl {{ARGS}}

# Update installed gpuicn components to the pinned registry (keeps local edits unless --overwrite)
ui-update *ARGS:
    gpuicn add avatar button dialog input menu progress scroll-area select separator sidebar slider tabs toast tooltip {{ARGS}}

# Show which gpuicn component files would be overwritten by an update
ui-diff:
    gpuicn add avatar button dialog input menu progress scroll-area select separator sidebar slider tabs toast tooltip --dry-run --overwrite

# Install the gpuicn CLI at the pinned release
ui-cli:
    cargo install gpuicn-cli --git https://github.com/devaryakjha/gpuicn --tag v0.5.0-beta.3 --locked

# Remove build output
clean:
    cargo clean
