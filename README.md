# TP-7 CLI

`tp7` is a macOS command-line tool for browsing, moving, and mounting files on a Teenage Engineering TP-7 field recorder without relying on FieldKit or Android File Transfer.

The TP-7 normally appears as a USB audio/MIDI device. `tp7` can send the device-specific MIDI mode switch, wait for the recorder to re-enumerate as MTP, open a direct MTP session, perform the file operation, and close the session again.

This is an unofficial project and is not affiliated with Teenage Engineering.

## What it does

`tp7` gives the terminal a practical file-access surface for the recorder. Common tasks:

```sh
tp7 devices
tp7 doctor
tp7 -a ls -lah /
tp7 -a ls -sS --human-readable /memo
tp7 -a stat /memo/2026-02-23_112713_000.wav
tp7 -a pull /memo/2026-02-23_112713_000.wav ./recordings/
tp7 -a pull /recordings ./recordings --recursive --skip-existing
tp7 -a push ./clip.wav /memo/clip.wav --dry-run
tp7 -a push ./clip.wav /memo/clip.wav --overwrite
tp7 -a rm /memo/clip.wav --dry-run
tp7 mount
tp7 unmount
```

For normal use, prefer `-a` / `--auto-connect`. Each command then handles the full TP-7 lifecycle: detect the recorder, switch to MTP if needed, open MTP, do the operation, and close cleanly.

`tp7 connect` and `tp7 eject` are diagnostic/manual-control commands. They are useful for checking whether MTP can be opened and released, but they are not a "mount once, run many commands, eject later" workflow.

`tp7 mount` exposes the TP-7 as a read-only Finder volume through a local WebDAV bridge backed by the same MTP session layer. With no mount point argument it chooses `/Volumes/TP-7`, or `/Volumes/TP-7 2` and so on when needed. The command stays running while the mount is active; use `tp7 unmount <mountpoint>` from another shell, or `tp7 unmount` to unmount all TP-7 mounts recorded by the CLI.

## Command surface

```text
devices  List connected TP-7 devices
doctor   Run macOS USB/MTP diagnostics
status   Show TP-7 device and storage status
connect  Switch or validate TP-7 MTP mode
ls       List files on the TP-7
tree     Show a recursive file tree
stat     Show metadata for one remote object
pull     Download a file or directory from the TP-7
push     Upload a file or directory to the TP-7
mkdir    Create a remote folder
rm       Delete a remote file or folder
rename   Rename a remote object without moving it
mount    Mount the TP-7 in Finder through a local WebDAV bridge
unmount  Unmount TP-7 WebDAV mount(s)
eject    Open and close an MTP session cleanly
```

Global flags:

```text
-a, --auto-connect     Switch the TP-7 into MTP mode when needed
-d, --device <SERIAL>  Select a TP-7 by serial number
-j, --json             Write machine-readable JSON output
    --no-progress      Disable progress output
-v, --verbose          Increase diagnostic logging
```

`ls` intentionally feels close to normal `ls` for the common cases:

```sh
tp7 -a ls -lah /memo
tp7 -a ls -sS --human-readable /
tp7 -a ls -ltr /recordings
```

## Agent-friendly by design

`tp7` follows the same Project Humane conventions as the other local tools in this workspace: concise text output for people, deterministic `--json` for scripts and agents, non-zero exit codes on errors, explicit destructive commands, and dry-run support where a command can change files.

The CLI is independent of companion apps. FieldKit was a useful research reference for understanding the TP-7 handshake, but it is not a runtime dependency.

## Transfer safety

Some TP-7 recordings can be very large. Use read-only inspection before pulling unknown files:

```sh
tp7 -a ls -lah /recordings
tp7 -a stat /recordings/take.wav
tp7 -a pull /recordings/take.wav ./recordings --dry-run
```

Human output shows readable sizes plus exact byte counts for `stat`, `pull`, and `push`. JSON output keeps numeric byte fields for scripts.

Downloads use temporary files and verify the final local size. Upload overwrite uses a staged remote replacement: the new file is uploaded under a temporary name before the existing object is renamed out of the way.

## Current TP-7 limitations

The TP-7 firmware tested here (`1.1.9`) accepts file upload, rename, delete, and download operations over MTP, but rejects MTP folder creation. Because of that:

- `mkdir` exists, but currently reports the TP-7 firmware rejection.
- `push --recursive` uploads into an existing remote folder tree only.
- Missing remote folders are detected before any recursive upload starts.

Finder mounting is currently read-only. It uses macOS' built-in WebDAV filesystem support and does not require macFUSE, Fuse-T, a kernel extension, or a system extension. Read-write mounting is intentionally not enabled yet because Finder-style writes need staged whole-file uploads and careful handling for safe-save, rename, delete, `.DS_Store`, and unsupported TP-7 folder creation behavior.

## Local requirements

- macOS
- A Teenage Engineering TP-7 connected over USB
- Rust 1.88 or newer for source builds and development

No Android File Transfer, FieldKit, libmtp, macFUSE, or kernel extension is required for the direct CLI or read-only Finder mount workflow.

## Install

With Homebrew:

```sh
brew tap totocaster/tap
brew install totocaster/tap/tp7
tp7 --version
```

From this repository:

```sh
cargo install --path . --locked
```

Or during development:

```sh
cargo run -- -a ls -lah /
```

## Development

Default smoke checks:

```sh
cargo fmt -- --check
cargo check
cargo clippy -- -D warnings
cargo test
cargo run -- --help
```

When the TP-7 is connected and write behavior changes, run the hardware smoke script. It creates only tiny temporary text files under `/memo` by default:

```sh
scripts/hardware-smoke.sh
```

Use `TP7_SMOKE_REMOTE_DIR=/existing/folder` to point it at another existing remote folder.

Protocol notes and implementation planning live in:

- `docs/spec.md`
- `docs/tp7-handshake.md`

## Release Process

Releases are tag-driven and update the Homebrew tap automatically:

1. Update the package version in `Cargo.toml`.
2. Commit the release change.
3. Tag the commit as `vX.Y.Z`.
4. Push the tag.

The GitHub `Release` workflow verifies formatting, check, clippy, tests, and a CLI smoke test, then builds `aarch64-apple-darwin` and `x86_64-apple-darwin` release archives. It publishes the GitHub release with install instructions, artifact checksums, and conventional-commit changelog notes, then rewrites `Formula/tp7.rb` in `totocaster/homebrew-tap` with the new artifact URLs and SHA256 sums. The `HOMEBREW_TAP_TOKEN` repository secret must be configured for the tap push.

## License

MIT. See `LICENSE`.

## Contributing and Security

Contribution guidelines are in `CONTRIBUTING.md`. For vulnerability reports or data-safety concerns, see `SECURITY.md`.
