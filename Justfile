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

# Format Rust and Nix files
fmt:
    cargo fmt
    nix fmt flake.nix

# Run all sandboxed checks: package, clippy, nextest, rustfmt
check:
    nix flake check

# Update Cargo.lock, respecting the dependency cooldown
update:
    cargo update

# Update flake inputs (nixpkgs, crane, rust-overlay)
update-flake:
    nix flake update

# Remove Cargo build artifacts
clean:
    cargo clean
