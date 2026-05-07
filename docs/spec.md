# TP-7 CLI Research Spec

## Purpose

Build a macOS CLI for Teenage Engineering TP-7 file access.

The implementation target is a robust Rust-based TP-7 file manager that can
detect the recorder, switch or validate MTP mode, list files, transfer
recordings, and optionally expose the MTP object store through Finder. Finder
mounting uses FUSE infrastructure because MTP is object-based and macOS has no
native Finder-level MTP support.

## Current Decision

Use Rust.

Recommended initial stack:

- `mtp-rs` for a pure Rust MTP implementation.
- `nusb` indirectly through `mtp-rs` for USB access.
- `clap` for CLI parsing.
- `tracing` or `log` for diagnostics.
- `fuser` plus macFUSE for Finder filesystem mounting.

Avoid `libmtp` in the first prototype unless `mtp-rs` cannot handle the TP-7 reliably. FieldKit uses `libmtp`, so it remains a proven fallback.

## Connected Device Findings

Observed on macOS with the TP-7 plugged in and powered on.

Device identity:

- Product: `TP-7`
- Vendor string: `teenage engineering`
- Vendor ID: `0x2367`
- Product ID: `0x0019`
- Serial: `F1RTL11C`
- USB speed: high speed, 480 Mbps
- USB version: 2.0
- Current USB configuration: `1`
- Number of USB configurations: `3`

Default visible mode is not MTP. macOS sees the TP-7 as a composite USB audio/MIDI device:

- Interface 0: USB Audio Control
- Interface 1: USB Audio Streaming input
- Interface 2: USB Audio Streaming output
- Interface 3: USB MIDI

Current owners:

- `usbaudiod` owns the audio interfaces.
- `MIDIServer` owns the MIDI interface and also appears as the device-level exclusive owner.
- `FieldKit.app` has a USB device user client attached.

A descriptor-only libusb probe showed all three available USB configurations expose the same audio/MIDI interface set in the current device mode. No MTP or mass-storage interface was visible at that point.

Confirmed: TP-7 needs a device-specific MIDI SysEx mode switch before it re-enumerates as MTP. The MTP interface is not merely hidden in another visible configuration while the device is in audio/MIDI mode.

After a successful switch, `nusb` sees:

- Product: `TP-7 MTP Device`
- Mode inference: `mtp`
- Interface: vendor-specific `255:1:1`, string `MTP TETP interface`

The TP-7 may return to audio/MIDI mode after a short MTP session closes. File commands should therefore switch, open MTP, perform their work, and close the session as one operation instead of assuming that a previous `tp7 connect` leaves the device in MTP for long.

## FieldKit Findings

The installed `/Applications/FieldKit.app` is strong evidence for how Teenage Engineering handles this on macOS.

Observed from local binary metadata:

- FieldKit version: `2.0`, bundle build `210`.
- It links against bundled `libmtp.dylib`.
- It links against bundled `libusb.dylib`.
- It has the `com.apple.security.device.usb` entitlement.
- It does not appear to bundle macFUSE/Fuse-T components.

Observed strings and logs indicate:

- FieldKit has a `MTPService`.
- FieldKit has USB probing code.
- FieldKit detects mass storage, MIDI, and MTP interfaces.
- FieldKit knows whether a device `supportsMTP` and `supportsStartingMTP`.
- FieldKit can re-enumerate the device.
- FieldKit sends MIDI `greet` and `mode` requests.
- FieldKit then opens the MTP device with libmtp.

Practical inference:

FieldKit sends a Teenage Engineering-specific MIDI request to switch the TP-7 from audio/MIDI mode into MTP mode, waits for USB re-enumeration, and then opens the re-enumerated MTP device through libmtp/libusb.

The exact mode-switch payload has been validated independently through CoreMIDI and implemented directly in Rust. FieldKit remains a research reference only, not a runtime dependency.

## Teenage Engineering SysEx Findings

TP-7 exposes a CoreMIDI source and destination named `TP-7` in audio/MIDI mode.

Device identity request:

- Send universal MIDI identity request: `f0 7e 7f 06 01 f7`
- Observed response: `f0 7e 19 06 02 00 20 76 19 00 01 00 00 00 00 00 f7`
- `0x19` is the runtime device id used in the TE SysEx envelope.

TE SysEx envelope:

- Manufacturer id: `00 20 76`
- TE marker byte: `40`
- Request with request id flag: `0x60`
- Response with request id flag: `0x20`
- Request id: 12 bits split as high 5 bits in the low bits of the flag byte and low 7 bits in the next byte.
- Payload is TE 8-bit data packed into 7-bit-safe SysEx bytes.

Generic request shape:

```text
f0 00 20 76 <device-id> 40 <flags> <request-id-low7> <command> <packed-payload> f7
```

Validated commands:

- `0x01` greet: returns semicolon-separated metadata.
- `0x04` mode: changes USB mode.

Observed greet request:

```text
f0 00 20 76 19 40 60 01 01 f7
```

Observed greet metadata:

```text
mode:normal;product:TP-7;sw_version:1.1.9;os_version:1.1.9;serial:F1RTL11C;sku:TE025AS001;base_sku:TE025AS001
```

Validated MTP switch request:

```text
f0 00 20 76 19 40 60 05 04 00 01 03 f7
```

That is command `0x04` with unpacked payload `[0x01, 0x03]`. FieldKit also contains a legacy-looking path that can send `[0x01, 0x02]`; keep it as a fallback if `[0x01, 0x03]` receives TE status `0x03` (`bad-request`).

Validated response to the MTP switch:

```text
f0 00 20 76 19 40 20 05 04 00 f7
```

Status `0x00` means success. After this response, the device re-enumerates and can be opened via `mtp-rs`.

## Protocol Findings

MTP is not a block storage protocol. It exposes object operations over a request/response protocol. A CLI can list, transfer, delete, and rename objects directly. A POSIX-looking mounted directory requires translation and caching.

Useful MTP concepts for implementation:

- Device sessions are explicit.
- Storage IDs identify storage roots.
- Object handles identify files/folders.
- Paths are not native protocol identifiers; the CLI must resolve paths to object handles.
- File writes are object uploads, not arbitrary block writes.
- Many POSIX write patterns do not map cleanly to MTP.
- Deletes and renames are protocol operations if supported by the device.

Core MTP operations likely needed for v1:

- Open session
- Get device info
- Get storage IDs
- Get storage info
- Get object handles
- Get object info
- Get object
- Send object info
- Send object
- Delete object
- Set object property or rename operation, depending on device support

## macOS Constraints

macOS has no native Finder-level MTP support. Teenage Engineering documents FieldKit as the macOS solution because Windows and Linux support MTP natively.

Potential process conflicts:

- FieldKit
- Android File Transfer or similar MTP apps
- `ptpcamerad`, if a device re-enumerates as a Still Image/PTP/MTP class device

The CLI should detect and report likely conflicts rather than failing with a generic USB error.

For mounting, macOS requires a FUSE-compatible runtime:

- macFUSE: mature, but requires kernel extension approval or newer FSKit paths depending on version/macOS.
- Fuse-T: kext-less, attractive for distribution, but needs validation with Rust FUSE tooling.

The Homebrew cask should install macFUSE automatically for normal users.
Source builds still need FUSE development metadata available at build time.

## Rust Tooling

Preferred:

- `mtp-rs`: pure Rust MTP implementation, built on `nusb`. This avoids C dependencies and should be the first prototype path.
- `fuser`: Rust FUSE library for a future mount layer.

Fallback:

- `libmtp` through FFI or a small C/Rust bridge. This is proven by FieldKit but introduces a C dependency and LGPL considerations.

Open validation tasks:

- Confirmed: `mtp-rs` can open the TP-7 once it is in MTP mode.
- Confirm object listing and transfers work with TP-7 recordings.
- Confirm whether `mtp-rs` exposes all required operations or whether we need small extensions.
- Confirm whether `nusb` can handle any macOS device ownership edge cases cleanly.

Validated MTP status from `mtp-rs`:

- Manufacturer: `teenage engineering`
- Model: `TP-7 MTP Device`
- Device version: `1.1.9`
- MTP serial: `F1RTL11C`
- Supports rename: `true`
- Storage count: `1`
- Storage id: `65537`
- Capacity: `120244404224` bytes
- Free space observed: `112986914816` bytes

## Go Tooling Considered

Go is feasible but less attractive for this project.

Options:

- `go-mtpfs`: useful as a historical reference for MTP/FUSE behavior.
- `go-fuse`: possible for a future mount, but macOS support is less straightforward.
- Go plus `libmtp` via cgo: likely robust but less appealing than pure Rust.

Decision: keep Go as reference material only.

## CLI Design

Executable name: `tp7`

Global flags:

- `-d, --device <serial>`: select a specific TP-7.
- `-j, --json`: machine-readable output.
- `--verbose`: show USB/MTP diagnostic logs.
- `-a, --auto-connect`: allow file commands to switch into MTP mode automatically.
- `--no-progress`: disable progress bars for scripts.

### V1 Commands

`tp7 devices`

List connected TP-7 devices. Show serial, USB mode, whether MTP appears available, and whether another app appears to own the device.

`tp7 status`

Show model, serial, firmware if available, battery if available, storage capacity/free space, and current mode.

`tp7 connect`

Switch the TP-7 into MTP mode if needed, wait for re-enumeration, and validate the MTP session. In v1, file commands should not silently switch modes unless `--auto-connect` is set.

`tp7 doctor`

Diagnose common macOS problems: TP-7 not found, FieldKit/Android File Transfer conflict, MTP mode unavailable, USB permission issues, and later FUSE availability.

`tp7 ls [remote-path]`

List files/folders. Default path is `/`.

Useful flags:

- `-l, --long`: detailed listing with type, size, and modified time.
- `-s, --size`: show object sizes in compact output.
- `-h, --human-readable`: format displayed sizes with units. `tp7 ls --help`
  remains available as long-only help.
- `-S, --sort-size`: sort by size, largest first.
- `-t, --sort-time`: sort by modified time, newest first.
- `-r, --reverse`: reverse listing order.
- `-i, --ids`: show MTP object IDs.
- `--json`

`tp7 tree [remote-path]`

Show recursive explorer output.

Useful flags:

- `--depth <n>`
- `--ids`
- `--json`

`tp7 stat <remote-path>`

Show metadata for one object: name, type, size, modified date, object ID, parent ID, and storage ID.
Human output shows size in readable units plus exact bytes.

`tp7 pull <remote-path> [local-path]`

Download a file or directory from the TP-7.
Human output shows transferred sizes in readable units plus exact bytes.

Useful flags:

- `--recursive`
- `--overwrite`
- `--skip-existing`
- `--dry-run`

`tp7 push <local-path> <remote-path>`

Upload a file or directory to the TP-7.
Human output shows transferred sizes in readable units plus exact bytes.

Useful flags:

- `--recursive`
- `--overwrite`
- `--dry-run`

Current directory upload is intentionally conservative: `--recursive` uploads
files into an existing remote folder tree. It does not create missing remote
folders because TP-7 firmware `1.1.9` rejected MTP folder creation during
hardware testing. Recursive push preflights the full local tree before writing
so missing remote folders or overwrite conflicts fail before any upload starts.
Without `--recursive`, local folder input returns a targeted `use --recursive`
error.

`tp7 mkdir <remote-path>`

Create a folder.

Useful flag:

- `--parents`

`tp7 rm <remote-path>`

Delete a file or directory.

Useful flags:

- `--recursive`
- `--force`
- `--dry-run`

`tp7 rename <remote-path> <new-name>`

Rename an object without moving it.

`tp7 eject`

Open and close an MTP session cleanly. This validates that the CLI can release
the device, but it does not force the TP-7 back to audio/MIDI mode. If the
reverse mode-switch command is discovered later, this can optionally use it.

`tp7 mount [mountpoint]`

Mount the TP-7 as a read-write Finder-visible filesystem. The command keeps
running as the userspace filesystem server. If the user unmounts the volume
from Finder, `diskutil`, or `umount`, the command exits cleanly and does not
depend on stored CLI state.

Implementation notes:

- With no mount point, the CLI uses `~/TP-7`. If that path is already mounted
  or unavailable, it tries numbered siblings such as `~/TP-7-2`.
- An explicit mount point is created when missing, but must be an empty
  directory before mounting.
- The Rust FUSE stack requires macFUSE/Fuse-T development metadata at build
  time on macOS and a working FUSE runtime at mount time.
- The implementation uses the same TP-7 MTP mode switch and open-session path
  as the direct file commands, then hands the opened MTP device to a FUSE
  filesystem for the lifetime of the mount.
- The mounted filesystem is read-write by default. `--read-only` keeps the mount
  inspection-only.
- TP-7 firmware `1.1.9` rejected MTP folder creation in direct CLI smoke tests,
  so Finder folder creation can fail even while file copy, overwrite, rename,
  and delete are writable.

`tp7 unmount [mountpoint]`

Stateless OS unmount wrapper. It does not read or write PID files or mount
registries. With no mount point, it inspects the current mount table and
unmounts the single mounted TP-7 volume. With an explicit path, it runs
`diskutil unmount`, falling back to `umount`; if the path is already unmounted,
it reports that without treating it as a CLI state error.

### Future Commands

`tp7 sync <remote-path> <local-path>`

One-way sync from TP-7 to Mac. This is likely the most useful workflow command for recordings.

`tp7 import [local-dir]`

Opinionated copy-new-recordings command with duplicate detection.

`tp7 mv <remote-source> <remote-dest>`

Remote move if TP-7 supports it reliably. Otherwise this must be copy/delete and should wait.

`tp7 cp <remote-source> <remote-dest>`

Remote copy if TP-7 supports it reliably.

`tp7 cat <remote-file>`

Stream a small remote file to stdout.

## Current Interface Decisions

Current choices:

- Keep `tp7 connect` explicit by default.
- Make `--auto-connect` opt-in for commands like `ls` and `pull`.
- Use POSIX-like remote paths beginning at `/`.
- Treat multiple storage roots as hidden behind `/` unless the device exposes more than one usable storage.
- Make destructive operations require explicit paths and support `--dry-run`.
- Retry initial TP-7 selection briefly because the device can be temporarily
  absent during USB re-enumeration after a previous MTP session closes.
- Retry transient CoreMIDI endpoint discovery during `--auto-connect` for the
  same 12-second window used for MTP visibility because the USB device can
  reappear before its MIDI endpoints are ready.
- Make Finder mounting read-write by default, with `--read-only` available for
  inspection-only sessions.

## Implementation Plan

Phases 0 through 4 now have working prototype coverage: CLI scaffolding, USB detection, diagnostics, CoreMIDI mode switching, MTP status validation, shared session handling, and read-only file exploration. Phase 5 has started with direct `tp7 pull` downloads.

### Phase 0: Baseline Project

Create a small Rust CLI skeleton with command parsing, structured errors, and logging. No MTP behavior yet.

Deliverable:

- `tp7 --help`
- command stubs
- basic JSON output mode

### Phase 1: USB Detection

Detect connected TP-7 devices by vendor/product ID and serial.

Deliverable:

- `tp7 devices`
- `tp7 doctor` can identify basic conflicts
- tests around output formatting where practical

### Phase 2: MTP Mode Discovery

Learn and implement the TP-7 mode-switch path.

Candidate approaches:

- First try manual FieldKit-assisted MTP mode, then see if our tool can open MTP.
- Inspect macOS logs around FieldKit mode switching.
- Capture CoreMIDI/USB MIDI SysEx traffic if needed.
- Reproduce the MIDI mode request once understood.

Deliverable:

- `tp7 connect` can put the TP-7 into MTP mode or clearly explain what manual step is needed.

### Phase 3: MTP Session

Open an MTP session with `mtp-rs` and read basic device/storage information.

Deliverable:

- `tp7 status`
- storage capacity/free-space reporting

### Phase 4: Read-Only File Explorer

Implement object tree traversal and remote path resolution.

Deliverable:

- `tp7 ls` (implemented for folder listings, file targets, `--long`, `--ids`, and `--json`)
- `tp7 tree` (implemented for recursive folder output, `--depth`, `--ids`, and `--json`)
- `tp7 stat` (implemented for file/folder object metadata and `--json`)

### Phase 5: Transfers

Implement downloads first, then uploads.

Deliverable:

- `tp7 pull` (implemented for files and recursive folders, with `--dry-run`, `--overwrite`, and `--skip-existing`)
- `tp7 push` (implemented for files and existing-folder recursive directory uploads, with `--dry-run`, staged `--overwrite`, and recursive preflight)
- progress reporting
- skip/overwrite behavior

### Phase 6: Mutations

Implement safer write operations after listing and transfer are proven.

Deliverable:

- `tp7 mkdir` (command implemented, but TP-7 firmware 1.1.9 rejected folder creation with MTP `GeneralError`)
- `tp7 rename` (implemented and smoke-tested on a pushed file)
- `tp7 rm` (implemented and smoke-tested on a pushed file)
- `tp7 eject` (implemented as MTP open/close validation)
- dry-run behavior

### Phase 7: Mount

Prototype a FUSE mount after the direct MTP CLI is stable.

Deliverable:

- `tp7 mount [mountpoint]` using `fuser`/`mtp-mount`
- Finder-visible read-write volume with `--read-only` support
- stateless `tp7 unmount [mountpoint]` wrapper

## Risks

Unknown TP-7 mode-switch request:

- FieldKit strongly suggests the mode switch is MIDI-based, but the payload is not known yet.

macOS USB ownership:

- Audio/MIDI mode is owned by system services.
- MTP mode may be claimed by other apps or macOS camera services.

MTP semantics:

- Object handles can change across sessions.
- Path resolution must be built on object metadata.
- Writes cannot behave exactly like normal filesystem writes.

Library risk:

- `mtp-rs` may need fixes or extensions for TP-7 quirks.
- `libmtp` fallback is likely reliable but adds C dependency and licensing considerations.

Mount risk:

- Finder-style mounting is a separate product surface with caching, metadata,
  macOS FUSE distribution concerns, and a build-time FUSE metadata dependency.
- Read-write Finder mounting remains risky because Finder write patterns and
  MTP object-write semantics do not match normal block filesystem behavior.

## References

- Teenage Engineering FieldKit guide: <https://teenage.engineering/guides/fieldkit>
- Teenage Engineering MTP guide: <https://teenage.engineering/guides/mtp>
- Teenage Engineering TP-7 product page: <https://teenage.engineering/products/tp-7>
- Teenage Engineering TP-7 downloads/release notes: <https://teenage.engineering/downloads/tp-7>
- Microsoft MTP extensions overview: <https://learn.microsoft.com/en-us/windows/win32/wpd_sdk/supporting-mtp-extensions>
- `mtp-rs` crate docs: <https://docs.rs/crate/mtp-rs/0.11.0>
- `fuser` crate docs: <https://docs.rs/fuser>
- macFUSE: <https://macfuse.github.io/>
- Fuse-T: <https://www.fuse-t.org/>
- `go-mtpfs`: <https://github.com/hanwen/go-mtpfs>
