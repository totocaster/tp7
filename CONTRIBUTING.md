# Contributing to TP-7 CLI

Thanks for considering a contribution. `tp7` is a small hardware-facing CLI, so changes should stay focused, well-tested, and careful around file transfer behavior.

## Getting Started

- Read the top-level `README.md` for user-facing behavior and install instructions.
- Review `docs/spec.md` and `docs/tp7-handshake.md` before changing protocol behavior.
- Use the pinned Rust toolchain from `rust-toolchain.toml`.
- Install macFUSE or Fuse-T development metadata before building the default
  macOS target, because Finder mounting is part of the normal binary.

## Development Workflow

Run the default smoke checks before opening a pull request:

```sh
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
cargo run -- --help
```

When the TP-7 is connected and write-path behavior changes, run the hardware smoke script:

```sh
scripts/hardware-smoke.sh
```

The hardware smoke creates only tiny generated files under `/memo` by default. Use `TP7_SMOKE_REMOTE_DIR=/existing/folder` to point it at another existing remote folder.

## Pull Requests

- Use conventional commit messages, such as `fix: guard overwrite path` or `docs: update handshake notes`.
- Keep PRs focused on one behavior change.
- Include tests for parser, path, transfer, and output behavior whenever possible.
- Update `README.md`, `docs/spec.md`, or `docs/tp7-handshake.md` when command behavior, protocol findings, supported workflows, or limitations change.

## Safety

Some TP-7 recordings can be large or irreplaceable. Do not pull, rewrite, or delete existing TP-7 recordings during development unless a test explicitly requires it and the operator has opted in. Prefer small generated files for write-path testing.

By contributing, you agree that your contributions are licensed under the MIT License.
