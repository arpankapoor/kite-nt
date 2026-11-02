test:
    cargo test --all-features

fmt:
    cargo +nightly fmt

docs:
    cargo doc --all-features --no-deps

clean:
    cargo clean

# Create a local release commit and annotated tag.
release version="minor":
    ./scripts/release.sh "{{ version }}"
