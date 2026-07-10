# Contributing to ctcx

Thanks for helping improve `ctcx`. Bug reports, design feedback, documentation fixes, and code contributions are welcome.

## Before you start

- Search existing issues before opening a new one.
- Open an issue before making a large change to the source schema, generated format, command-line interface, or compatibility guarantees.
- Keep pull requests focused on one coherent change.
- Follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Development setup

Install a stable Rust toolchain with the `rustfmt` and `clippy` components, then clone and validate the project:

```bash
git clone https://github.com/benkleyner/ctcx.git
cd ctcx
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Making a change

1. Create a branch from `main`.
2. Add tests for behavior changes and regression fixes.
3. Update the README when changing public commands, configuration, output, or guarantees.
4. Run the complete validation suite.
5. Open a pull request using the repository template.

Tests that exercise the CLI should use temporary directories and must not depend on a user's global Git or GitHub configuration.

## Design expectations

- Generated output must be deterministic.
- Diagnostics should identify the relevant source file and rule when possible.
- New syntax must remain strict and versioned.
- File operations must preserve project-root confinement and safe-overwrite behavior.
- Avoid adding remote execution, templating, or implicit precedence without a separately reviewed design.

## Licensing

Unless explicitly stated otherwise, contributions submitted to `ctcx` are licensed under either the MIT License or Apache License 2.0, at the user's option, without additional terms.
