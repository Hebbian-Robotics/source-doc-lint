# Contributing

Build from the repository root with stable Rust and a platform C compiler.
GitHub Issues is the place to report bugs or propose changes. For a detection
bug, include the smallest source snippet and configuration that reproduce it,
plus the expected finding. Open pull requests against `main`.

## Checks

Install the dependency and link checkers once.

```sh
cargo install cargo-machete --version 0.9.2 --locked
cargo install cargo-deny --version 0.20.2 --locked
cargo install lychee --version 0.24.2 --locked
```

From the repository root, run the following commands.

```sh
cargo fmt
cargo machete --with-metadata --skip-target-dir
cargo deny check bans
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
lychee --offline --include-fragments --extensions md .
```

Keep lint behavior changes covered by the command-line fixtures. CI runs the
Rust checks on Linux, macOS, and Windows. The default system linker works on
each platform. On Linux, contributors can opt into mold by setting
`RUSTFLAGS="-C link-arg=-fuse-ld=mold"` after installing it.

## Code Style & Philosophy

### Typing & Pattern Matching

- Prefer **explicit types** over raw dicts -- make invalid states unrepresentable where practical
- Prefer **typed variants over string literals** when the set of valid values is known
- Use **exhaustive pattern matching** (`match` in Python and Rust, `ts-pattern` in TypeScript) so the type checker can verify all cases are handled
- Structure types to enable exhaustive matching when handling variants
- Prefer **shared internal functions over factory patterns** when extracting common logic from hooks or functions -- keep each export explicitly defined for better IDE navigation and readability

### Forward Compatibility

- **Unknown values**: Parse to an explicit `Unknown*` variant (never `None`), log at warn level, preserve raw data, gracefully ignore instead of raising exception

### Self-Documenting Code

- **Verbose naming**: Variable and function naming should read like documentation
- **Strategic comments**: Only for non-obvious logic or architectural decisions; avoid restating what code shows
