# Another machine

Using the daemon on another machine, your desktop from your laptop or a server, with its terminals running there and carrying on when you disconnect. This lesson is to read; the terminal beside it is for trying the commands, if you've a machine to try them on.

```setup
terminal
```

## A window on another machine

`clackatoo --remote HOST` opens a window on HOST's workspaces, through ssh: a hostname, `user@host`, or a name from your `~/.ssh/config`. The `clackatoo` command takes `--remote` too: `clackatoo list --remote desktop` lists its terminals.

It runs `ssh HOST clackatoo bridge`, which starts the daemon there if it isn't running. ssh does the logging in, and the daemon only takes connections that come through it.

## Picking a machine

`clackatoo --connect-picker` opens the window as a picker of the machine to connect to: those you've connected to lately, newest first, those your `~/.ssh/config` names, and Tailscale's peers that are online. Type a few letters to narrow them, or a machine that isn't listed, as `jack@server:2222`, and Enter connects, as `--remote` would. If it can't, the picker opens again saying why, so you can pick another; Esc closes the window. Your app launcher runs it as "Connect to…" when you right-click clackatoo, and offers the last three machines you connected to beside it.

In a window, `:connect`, or Ctrl+B r, opens the same picker over your panes, and picking a machine opens a new window on it, leaving this one as it is.

## What the other machine needs

clackatoo, of the same release or one a little older or newer. A window and a daemon a release or so apart talk at the older one's protocol, so your laptop's window keeps working when your desktop's daemon is updated before it, and the other way round. Panes of a kind the window doesn't have yet say they need a newer clackatoo, and what it offers that the daemon doesn't have yet says the daemon needs updating, naming its release. Further apart, a window meeting a daemon from a newer build offers to update this clackatoo to the latest release, and opens again as it, connected where it was; one meeting an older daemon offers to upgrade it in place. The other machine needs clackatoo on the `PATH` ssh's commands get, which is often shorter than your shell's: `--remote-command` names it otherwise, as `--remote-command ~/.cargo/bin/clackatoo`. On a server nobody's logged in to, run `loginctl enable-linger` there once, so the daemon's socket stays.

## Surviving the network

Once connected, the connection moves out of ssh onto UDP if it can, so it carries on when your network changes or your laptop sleeps a moment, and when ssh dies a new ssh connection is opened behind it. A firewall on the other machine has to let UDP in on ports 60000 to 61000, as for mosh. `clackatoo ping --remote HOST` says how long the daemon takes to answer, and which way each round went.

When the other machine stops answering, a red notice in the top bar counts the seconds. Editors go on editing, and what you type in them is sent once the connection's back; keys for terminals are turned down, with "not connected: keys aren't sent", rather than reach a shell long after you meant them. If the connection drops, as when the daemon there is upgraded, the window connects again by itself, keeping its panes and what you typed into editors, with "Reconnecting" in the top bar meanwhile. Nothing on the other machine stops.

## From your phone

The daemon can serve a web app, for your phone or any browser: your chats by workspace, with those that need you first, and each chat as a window shows it, with its questions, the box to send a message, Stop, the model, mode and effort menus, and the cards of the chats its agent started. It's off until `~/.config/clackatoo/daemon/web.toml` says `mode = "tailscale"`, which listens on a socket only you can reach, `~/.local/share/clackatoo/web/web.sock`, for `tailscale serve --bg unix:` and that path to give your tailnet, over HTTPS; `mode = "public"` serves it to the network, with your own certificate and a password.

A phone pairs once: with the password `clackatoo web password` sets, or by asking a window. Asking shows a code on the phone, and the window's bar says a device asks; type `:Devices allow` and the code to let it in, or `:Devices deny`. `:Devices` lists what's paired, and `:Devices revoke 2` unpairs the second, cutting it off at once. After fifty wrong passwords in a row, pairing with the password locks, the bar says so, and `:Devices unlock` lets it again. Turn on notifications in the app, and the phone is told when a chat finishes, or asks you something, while no window you're using shows it. On an iPhone, add the app to the home screen first (Share, then Add to Home Screen), and pair it from there: only an app on the home screen gets notifications.

## Terminals from your phone

The app lists each workspace's terminals under its chats, with what each is running and where. Tap one to see it as it is, at its own size: its rows as wide as the phone allows, and if they're wider still, panned sideways with a finger; pinch to zoom. Scroll up for its history. **Fit** sizes the terminal to the phone instead, and says Fitted while it lasts; it goes back to the size your windows want when you tap Fitted, leave the terminal, or type into it from a window.

Tap the rows to bring up the keyboard; what you type goes as you type it, a key at a time, and the row above the keyboard has Esc, Tab, Ctrl (for the next key), Ctrl+C, the arrows, `|`, `~`, `/`, `-` and Paste. When the daemon can't be reached, or hasn't answered for three seconds, keys are turned down and the phone says so, rather than reach a shell long after you meant them.

## Panes from several machines

`:connect HOST`, to bring another machine's workspaces into this daemon, and `:Terminal@host`, for a pane on another machine beside your own, aren't built yet, as both say. Until then, a window shows one machine, and `:connect` alone opens a window on another.
