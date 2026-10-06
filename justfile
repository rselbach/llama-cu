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

# Build and sign target/release/llama-cu.app.
app:
    scripts/build-app.sh

# Install llama-cu.app into ~/Applications and link the command into
# ~/.cargo/bin. The app owns llama-cu's permissions.
install: app
    mkdir -p ~/Applications ~/.cargo/bin
    rm -rf ~/Applications/llama-cu.app
    ditto target/release/llama-cu.app ~/Applications/llama-cu.app
    ln -sfn ~/Applications/llama-cu.app/Contents/MacOS/llama-cu ~/.cargo/bin/llama-cu

# Link the agent skill into ~/.agents/skills, where Pi finds it.
install-skill:
    mkdir -p ~/.agents/skills
    ln -sfn "{{justfile_directory()}}/skills/llama-cu" ~/.agents/skills/llama-cu
