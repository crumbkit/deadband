# Working on deadband

deadband keeps a busy machine usable while many heavy jobs compete for it.
It is a Cargo workspace with one library crate, `crates/deadband`. The rules
are the Rust standards that [rust-standards.toml](rust-standards.toml)
adopts. The check suite `/suites/deadband-pr-lane` runs the pull-request
lane, and a change is ready when the lane passes:

```sh
export CARGO_BUILD_WARNINGS=deny
cargo fmt --check
zizmor .
cargo deny --locked check bans licenses sources
cargo shear
cargo clippy --locked --workspace --all-targets --all-features
cargo nextest run --locked --workspace --all-features
cargo test --locked --doc --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --no-deps
cargo hack clippy --locked --workspace --each-feature --all-targets
cargo +1.95.0 check --locked --workspace --all-targets --all-features
```

- Write the test first, named as a sentence (BASE-36).
- Give each new dependency a reason in `Cargo.toml` (BASE-23).

## Writing

Docs, comments and commit messages use plain declarative sentences with a
named subject. Avoid "not X but Y", fragments, metaphors and closing
flourishes. A stated limit says what it counts and what happens when it is
hit. Commit messages describe changed behaviour.
