default:
    @just --list

# Run the app in debug mode
run *args:
    cargo run -- {{ args }}

# Build the release package with Nix, without a result link
build:
    nix build --no-link --print-out-paths

# Run clippy like the flake check does
lint:
    cargo clippy --all-targets -- --deny warnings

# Run tests with nextest like the flake check does; extra args go to nextest
test *args:
    cargo nextest run --no-tests=warn {{ args }}

# Run the tests that need a phone, in a new folder inside `url` (mtp:/DEVICE/STORAGE/DIR)
test-mtp url:
    MUSIC_SYNCER_TEST_MTP={{ quote(url) }} cargo nextest run --run-ignored only --test-threads 1 mtp::tests

# Format Rust and Nix files (git hooks)
fmt:
    nix fmt

# Run all sandboxed checks: package, clippy, nextest, git hooks
check:
    nix flake check

# Update Cargo.lock
update:
    cargo update

# Update flake inputs (nixpkgs, crane, rust-overlay, git-hooks)
update-flake:
    nix flake update

# Remove Cargo build artifacts
clean:
    cargo clean
