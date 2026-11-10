---
title: Secret Regions
type: reference
description: OSC 46373, the escape sequence a program uses to mark what it prints as secret, never to be saved, or kept from agents. The sequence, the rules a terminal keeps, what clackatoo does with each hint, and how to use it from a shell.
related:
  - clackatoo/reference/terminal-emulation
  - clackatoo/reference/workspaces
  - clackatoo/reference/agent-tools
  - clackatoo/reference/config
---

A program can mark part of what it prints as private: a password a tool
shows once, a token, a key. The terminal is told with an escape sequence
before and after it, and treats what's between by the **hints** the program
gives. Each hint only ever adds protection. Nothing a program prints can take
one away, or uncover what another marked.

This page is the sequence's specification, written so other terminals can
adopt it, and what clackatoo does with it.

## The sequence

```
OSC 46373 ; region ; start ; ID ; FLAGS ST
OSC 46373 ; region ; end ; ID ST
OSC 46373 ; forget ST
```

`OSC` is `ESC ]` and `ST` is `ESC \`; `BEL` ends it too, as for any OSC.

- **`start`** opens a region. Every line printed on while it's open gets its
  hints.
- **`end`** closes the newest open region whose ID is `ID`.
- **`forget`** asks the terminal to forget what it has kept of itself on disk,
  and what it shows now: what's printed after is kept as before. Any
  program can ask it, so any program can wipe what's kept of the terminal
  it runs in; it only ever keeps less.

**`ID`** is 1 to 64 letters, digits, `-`, `_` or `.`. Whoever opens a region
picks it at random, at least 16 random bytes in hex, and never shows it. Only
the same ID ends the region, so nothing printed inside it, which never sees
the ID, can end it early.

**`FLAGS`** is a list of hints, split by commas:

| Flag | Means |
|---|---|
| `hide` | Masked on screen until the user asks to see it. |
| `nosave` | Never written to disk. |
| `noagent` | Never shown to AI agents reading the terminal. |
| `secret` | All three. |
| `private` | `nosave` and `noagent`: shown, but kept nowhere and from no one but the user. |

A flag a terminal doesn't know is ignored, so the list can grow. A region
with no flag the terminal knows, empty flags or none at all
(`OSC 46373 ; region ; start ; ID ST`), fails closed: it's `secret`.
There's no flag to keep something: a program can never weaken a choice the
user made about what's kept.

The terminal answers none of these, and nothing is echoed back.

No other terminal or program we know of uses OSC 46373. clackatoo
v2026.11.36 alone used OSC 3008 for these; that's systemd's context
signalling (`OSC 3008 ; start=…`), so it moved, and clackatoo no longer
reads OSC 3008 at all.

### Rules a terminal keeps

1. **Hints go by line.** A line printed on while a region is open gets the
   hints of every open region, and keeps them for as long as it's kept: in
   history, through rewrapping on a resize, across an upgrade. Text a later
   program moves onto another line, as scrolling within margins or
   inserting lines does, takes them there too. A line written over later
   keeps them, until it's cleared.
2. **An end must carry its start's ID.** Regions nest: an end closes the
   newest open one with its ID, and leaves the others open.
3. **The shell's next prompt closes every region,** so a program can't
   leave one open over the user's later commands. Only the shell's own prompt
   counts, never one a program prints, so a terminal must be sure of it.
   clackatoo's shell integration signs it: it prints
   `OSC 46373 ; prompt ; clackatoo=SECRET ST` with each prompt, with a secret
   only the shell and clackatoo know, never in its environment. Nothing
   else closes a region at a prompt: not OSC 133's marks, which any program,
   or a shell nested in one, as over `ssh`, can print. The shell's next
   command, signed the same way, closes them too.

   The secret is the shell's, so anything running in the shell itself can
   read it: its startup files, its functions, whatever it sources. In fish,
   which runs clackatoo's integration after its startup files, so can a
   program `config.fish` starts, while the file that hands fish the secret
   is still there. A program the shell runs never can.
4. **Without shell integration, a region closes after 1,000 lines,**
   each counted once however often it's printed on. No prompt will. With
   it, after 100,000, for a shell that never prompts again, as one
   replaced by `exec` or nested in another.
5. **A start is never dropped.** Past 16 regions open at once, or with an ID
   that isn't one, a start's hints still apply, until the prompt or the cap.
6. **Nothing else closes a region:** not a reset (`ESC c`), nor the alternate
   screen.

A region closed early only leaves out what's printed after. What was printed
in it keeps its hints.

7. **Inside a region kept from agents (`noagent`),** a program's title and
   working directory (OSC 0, 2 and 7), its notifications and its progress
   (OSC 9, 777 and 99) are ignored: they go where agents and the phone see
   them.

## In clackatoo

### `hide`: masked

A hidden line is shown as `●●●●`, dim, whatever it held. Each row of a
long line hidden is a row of the mask, so how many rows it took still
shows, but not where its text ends; while the cursor is on one, it's
hidden. The daemon masks it in everything it sends: the window, a [window
on another machine](~clackatoo/reference/remote), a daemon relaying, the
[phone](~clackatoo/reference/phone), and windows from before this release
alike. So what you copy or yank of it is the mask too, and a search (`/`,
`?`, `n`) never goes to it, nor lights it. A picture a program shows on a
hidden line ([images](~clackatoo/reference/images)) is never sent, nor is
its image, even once you ask to see the line's text; on a line kept from
agents, it's sent only where agents can't see it, and copies and agents'
reads say nothing of it.

To see it, put the cursor on it in normal mode and press `zv`, or
<kbd>Ctrl</kbd>+click it. The run of hidden lines around it is shown as it is
in that window, and copies and searches there see it, for 30 seconds; `zv`
again masks it at once, and a line printed on afresh is masked again. Only a
window that reaches the terminal's daemon itself can: on its machine, or with
`--remote`. A window through `:connect` can't, as the daemon relaying would
show the lines to all its clients, phones too; nor can the phone, nor an
agent driving a window (`:agentaccess`). In a window an agent can see,
what you ask to see is only what's hidden, never what's kept from agents
too.

### `nosave`: never on disk

A `nosave` line is never written to the [history kept across a
restart](~clackatoo/reference/workspaces#scrollback-after-a-restart): a run
of them comes back as one line, `[secret output not kept]`. `forget` does
what [`:forget`](~clackatoo/reference/workspaces#scrollback-after-a-restart)
does.

[Recordings](~clackatoo/reference/recordings) are a byte-for-byte copy of
what a program printed, regions and all: a recorded terminal keeps
everything, as you asked.

`nosave` is about the disk only: a `nosave` line is shown to agents unless
it's `noagent` too, as `secret` and `private` are.

### `noagent`: kept from agents

What an agent reads of a terminal, with `read_terminal`, by waiting for a
command, or in what a command it ran printed, has `[secret]` in place of
each run of `noagent` lines. A window that has given an agent control of
itself (`:agentaccess`) masks them too, as the agent sees what it draws.

Windows through `:connect` are sent `noagent` lines masked, so they never
leave the terminal's machine as they are: the daemon relaying could show
them to an agent there.

Anything else that asks the daemon for a terminal without showing it, such
as `clackatoo capture`, which a command an agent runs could run, is sent
`noagent` lines masked; `clackatoo capture --include-private` asks for them
as a window showing the terminal does. That's a guard, not a boundary: any
program running as you can reach the daemon's socket, as a window does.

### Passwords typed unseen

A line you type a password on, at a prompt that doesn't show it (`sudo`,
`ssh`, `read -s`: anything reading a line with echo off), is marked
`nosave` and `noagent` by itself, as is one an agent types on. Nothing typed
there shows, but a program may show stars, or what it was told.

It's told by the terminal's settings as you type, so it misses:

- prompts drawn by a full-screen program, or any reading keys one at a
  time (raw mode), which draws what's typed itself;
- `sudo` on a machine you `ssh` into, or inside `tmux` or `screen`, where
  the terminal clackatoo sees is `ssh`'s or theirs, which reads keys raw;
- what you type before the prompt has turned echo off (typing ahead).

### Config

In the daemon's config (see [config](~clackatoo/reference/config)):

| Option | Default | What it does |
|---|---|---|
| `daemon.hints.hide` | `"mask"` | `"show"` sends hidden lines as they are. |
| `daemon.hints.nosave` | `true` | `false` writes `nosave` lines to disk too. |
| `daemon.hints.noagent` | `"hide"` | `"show"` lets agents read `noagent` lines. |
| `daemon.hints.shown_for` | `30` | How many seconds lines you asked to see stay shown. |

## Using it

`clackatoo secret` runs a command with what it prints in a region, its
output and its errors, with a fresh random ID, closed as the command ends,
even by <kbd>Ctrl</kbd>+<kbd>C</kbd>, a hangup or `kill`, after which it
ends by that signal too. Stopped with <kbd>Ctrl</kbd>+<kbd>Z</kbd> and
brought back with `fg`, it opens a fresh region; but the command carries on
at the same moment, so what it prints before then lands outside it. Its
output piped, or sent to a file, it marks nothing, as nothing it prints
reaches the terminal; its errors, if they do, still are. Killed outright
(`kill -9`), it closes nothing: the shell's next prompt does, or without
shell integration, the cap.

```sh
clackatoo secret -- pass show github/token
clackatoo secret --private -- aws sts get-session-token
clackatoo secret --hide-only -- cat ~/.config/hub
```

Where clackatoo isn't installed, as on a machine you `ssh` into, a shell
function does the same:

```bash
# bash or zsh
secret() {
    local id rc
    id=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
    printf '\e]46373;region;start;%s;secret\e\\' "$id"
    # Ctrl+C still ends the region.
    trap 'printf "\e]46373;region;end;%s\e\\\\" "$id"; trap - INT; return 130' INT
    "$@"
    rc=$?
    trap - INT
    printf '\e]46373;region;end;%s\e\\' "$id"
    return $rc
}
```

```fish
function secret
    set -l id (od -An -N16 -tx1 /dev/urandom | string replace -a ' ' '' | string join '')
    printf '\e]46373;region;start;%s;secret\e\\' $id
    # Should Ctrl+C stop it here, the shell's next prompt ends the region.
    $argv
    set -l rc $status
    printf '\e]46373;region;end;%s\e\\' $id
    return $rc
end
```

A terminal that doesn't know OSC 46373 ignores it, as it ignores any OSC it
doesn't know, so these are safe anywhere.
