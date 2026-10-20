---
title: Recordings
type: reference
description: The recording format clackatoo new --record writes. It's a text header with the terminal's size, then the raw output, and replaying it rebuilds the same screen.
related:
  - clackatoo/reference/cli
---

`clackatoo new --record <FILE>` saves everything the terminal's program prints. Replaying a recording through clackatoo's emulator, at the recorded size, rebuilds the same screen every time, however the bytes are split up. clackatoo's tests are built on recordings of real programs, and its throughput benchmarks replay one too.

## Format

A single header line, then the program's output, byte for byte:

```text
clackatoo-recording v1 cols=80 rows=24
<raw output…>
```

The header is UTF-8 text ending in a newline. It starts with `clackatoo-recording v1`, followed by space-separated `key=value` fields:

| Field | |
|---|---|
| `cols` | The terminal's width. Required. |
| `rows` | The terminal's height. Required. |

Readers ignore fields they don't know, so later versions can add fields without breaking v1 readers. Everything after the header's newline is output, including any further newlines.

The output is written as it arrives, so a recording can be read while the program is still running. Input typed into the terminal is not recorded. A recording has one size, so resizing the terminal stops it.

## Privacy

A recording contains everything the program displayed: paths, hostnames, file contents and anything else that appeared on screen. Check a recording before sharing it.
