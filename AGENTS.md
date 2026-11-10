# Working on deadband

deadband keeps a busy machine usable while many heavy jobs compete for it.
It is a Cargo workspace with one library crate, `crates/deadband`. The rules
are the Rust standards that [rust-standards.toml](rust-standards.toml)
adopts. A change is ready when the pull-request lane passes. The commands
below are the lane, and the check suite `/suites/deadband-pr-lane` runs the
same commands:

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
cargo crumbkit skill check --bin deadband
cargo crumbkit skill draft --bin deadband --check
```

- Write the test first, named as a sentence (BASE-36).
- Give each new dependency a reason in `Cargo.toml` (BASE-23).

Read [docs/architecture.md](docs/architecture.md) for the modules and the rules
that shape them.

## Writing

Docs, comments and commit messages use plain declarative sentences with a
named subject. Avoid "not X but Y", fragments, metaphors and closing
flourishes. A stated limit says what it counts and what happens when it is
hit. Commit messages describe changed behaviour.
