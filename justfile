default: check

# Build an optimized binary.
build:
    cargo build --release

# Run unit tests.
test:
    cargo test

# Lint with clippy, treating warnings as errors.
lint:
    cargo clippy --all-targets -- -D warnings

# Format the code.
fmt:
    cargo fmt

# Check formatting, lint, and test.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

# Install llama-cu into ~/.cargo/bin.
install:
    cargo install --path .

# Link the agent skill into ~/.agents/skills, where Pi finds it.
install-skill:
    mkdir -p ~/.agents/skills
    ln -sfn "{{justfile_directory()}}/skills/llama-cu" ~/.agents/skills/llama-cu
