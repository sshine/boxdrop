# Default recipe: list available commands
default:
    @just --list

# Format all code (Rust, Nix, Markdown)
fmt:
    treefmt

# Check formatting
fmt-check:
    treefmt --fail-on-change --no-cache

# Clippy over the workspace
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run the test suite
test:
    cargo test --workspace --all-features

# Build the workspace
build:
    cargo build --workspace

# Build the Nix package
nix-build:
    nix build .#default --no-link --print-out-paths

# Run the deterministic simulation
sim:
    cargo run -p boxdrop-harness -- sim

# Run the two-host user-namespace harness
harness:
    cargo run -p boxdrop-harness -- run

# Perpetual soak with bounded disk usage
soak duration="10m" high="512MiB" low="256MiB":
    cargo run -p boxdrop-harness -- soak --duration {{duration}} --high-water {{high}} --low-water {{low}}

# Everything CI runs
ci: fmt-check lint test nix-build
    @echo "All CI checks passed"
