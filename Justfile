test:
    cargo test --all-features

fmt:
    cargo +nightly fmt

docs:
    cargo doc --all-features --no-deps

clean:
    cargo clean
