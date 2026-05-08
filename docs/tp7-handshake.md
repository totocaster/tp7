# TP-7 MTP Handshake

This document summarizes how the TP-7 switches from its normal USB audio/MIDI
mode into MTP mode, and how we reverse-engineered the sequence.

## Short Version

The TP-7 does not expose MTP as a normal macOS mountable device when first
plugged in. It starts as a USB audio/MIDI device. To access files, the CLI must:

1. Find the TP-7 over USB and CoreMIDI.
2. Send a Teenage Engineering SysEx `greet` command over MIDI.
3. Send a Teenage Engineering SysEx `mode` command over MIDI.
4. Wait for the TP-7 to re-enumerate as `TP-7 MTP Device`.
5. Open an MTP session and operate on files before the device returns to
   audio/MIDI mode.

FieldKit was a useful research reference, but the CLI must not depend on it at
runtime.

## Runtime Handshake

```mermaid
sequenceDiagram
    participant CLI as tp7 CLI
    participant USB as macOS USB
    participant MIDI as CoreMIDI
    participant TP7 as TP-7
    participant MTP as MTP session

    CLI->>USB: Detect vendor 0x2367, product 0x0019
    USB-->>CLI: TP-7 in audio/MIDI mode
    CLI->>MIDI: Find TP-7 MIDI source and destination
    CLI->>TP7: Universal MIDI identity request
    TP7-->>CLI: Identity response with device id 0x19
    CLI->>TP7: TE SysEx greet command 0x01
    TP7-->>CLI: Product, firmware, serial, mode metadata
    CLI->>TP7: TE SysEx mode command 0x04, payload [0x01, 0x03]
    TP7-->>CLI: Success status 0x00
    TP7->>USB: Disconnect and re-enumerate
    USB-->>CLI: TP-7 MTP Device, MTP TETP interface
    CLI->>MTP: Open MTP session
    MTP-->>CLI: Storage and object access
```

## Device States

```mermaid
stateDiagram-v2
    [*] --> AudioMidi: USB plug-in
    AudioMidi --> Switching: TE SysEx mode command
    Switching --> MTP: USB re-enumeration
    MTP --> AudioMidi: Session closes or device times out
    MTP --> MTP: File commands while session is open
```

## SysEx Shape

The generic Teenage Engineering SysEx envelope is:

```text
f0 00 20 76 <device-id> 40 <flags> <request-id-low7> <command> <packed-payload> f7
```

Important bytes:

- Manufacturer id: `00 20 76`
- Device id observed from TP-7: `0x19`
- TE marker byte: `0x40`
- Request flag shape: `0x60`
- Response flag shape: `0x20`
- Payload bytes are packed into 7-bit-safe SysEx data.

Validated messages:

```text
Universal identity request:
f0 7e 7f 06 01 f7

Observed identity response:
f0 7e 19 06 02 00 20 76 19 00 01 00 00 00 00 00 f7

Greet request:
f0 00 20 76 19 40 60 01 01 f7

MTP mode switch request:
f0 00 20 76 19 40 60 05 04 00 01 03 f7

MTP mode switch success response:
f0 00 20 76 19 40 20 05 04 00 f7
```

The successful greet response included:

```text
mode:normal;product:TP-7;sw_version:1.1.9;os_version:1.1.9;serial:F1RTL11C;sku:TE025AS001;base_sku:TE025AS001
```

After the MTP switch, the TP-7 re-enumerated as:

```text
Product: TP-7 MTP Device
Interface: MTP TETP interface
Manufacturer: teenage engineering
Model: TP-7 MTP Device
Storage count: 1
```

## Reverse-Engineering Path

```mermaid
flowchart TD
    A[USB enumeration] --> B[Confirmed default audio/MIDI mode]
    B --> C[IORegistry ownership checks]
    C --> D[FieldKit process behavior]
    D --> E[FieldKit strings and symbols]
    E --> F[TE web updater SysEx format]
    F --> G[CoreMIDI identity and greet probe]
    G --> H[FieldKit connectMTP analysis]
    H --> I[Mode command 0x04 with payload 0x01 0x03]
    I --> J[TP-7 re-enumerates as MTP]
    J --> K[mtp-rs opens storage successfully]
```

What each step taught us:

- USB enumeration showed no visible MTP interface in the default state.
- IORegistry showed audio, MIDI, and competing app ownership.
- FieldKit strings pointed to `MTPService`, `MIDIClient`, `greet`, `mode`,
  `sendMidiGreet`, `connectMTP`, `TeSysExCommand`, and `TeSysExMessage`.
- The official TE updater JavaScript explained the shared SysEx envelope,
  request IDs, responses, and 7-bit payload packing.
- CoreMIDI probes proved the TP-7 accepts TE SysEx directly.
- FieldKit analysis revealed the TP-7 MTP switch command and payload.
- `mtp-rs` proved the resulting device can be accessed without FieldKit.

## CLI Implications

- File commands switch to MTP when `--auto-connect` is set, open a session,
  perform the operation, and close cleanly in one flow.
- `tp7 status` cannot assume a previous `tp7 connect` left the TP-7 in MTP mode.
- The CLI should warn when FieldKit or other processes own TP-7 interfaces,
  but it should not require those apps.
- Newer firmware accepts mode payload `[0x01, 0x03]`. FieldKit suggests
  `[0x01, 0x02]` may be a fallback for older firmware.
- MTP support lives behind our own session layer. `ls`, `tree`, `stat`, and
  `pull`, `push`, `rename`, and `rm` now use that same switch/open/work/close
  flow.
- `mount` keeps a long-lived MTP session open and exposes it to Finder through a
  local read-only WebDAV server plus macOS `mount_webdav`; `unmount` tears down
  one recorded mount or all recorded TP-7 mounts.
- TP-7 firmware `1.1.9` accepted file upload, rename, and delete in smoke tests,
  but rejected folder creation with MTP `GeneralError`.
- `push --overwrite` stages a replacement under a temporary remote name before
  it renames the old object out of the way, so the original file remains intact
  until the replacement upload succeeds.
- Recursive `push` only writes into remote folders that already exist. Missing
  remote folders remain unsupported until we find a reliable TP-7 folder-create
  path.
- Recursive `push` preflights the whole local tree before upload, so a missing
  remote folder or overwrite conflict fails before any file is written.
- `eject` is currently an MTP open/close validation command. It releases the
  session cleanly but does not send a reverse mode-switch command.
- The shared session layer retries initial TP-7 selection briefly because the
  device can disappear for a moment while returning from MTP to audio/MIDI mode.
- Auto-connect also retries transient CoreMIDI endpoint discovery for up to the
  same 12-second window used for MTP visibility because USB audio/MIDI mode can
  appear before the TP-7 MIDI endpoints are ready.
