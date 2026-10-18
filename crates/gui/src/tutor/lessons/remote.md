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

## Forwarding a port

A dev server on the other machine that listens on `localhost:8080` can't be reached from this machine's browser. `:PortForward 8080` forwards it: this machine's `localhost:8080` then reaches the other machine's, through the window's own connection, so it survives the network changing as the window does. `:PortForward 3000:8080` uses port 3000 here, as when 8080 is taken here, which the bar says, suggesting a free port; `:PortForward 3000:box:8080` reaches a machine the other one can, as `box:8080`. Only this machine can connect to the ports forwarded.

`:PortForward` alone lists the forwards and how many connections each has open, and `:PortForward! 8080` stops one, or with no port, all of them. They're kept while the window is open: when the connection drops, or the daemon there is upgraded, connections open through them are cut off, and new ones wait until the window has connected again. If nothing listens at the other end, the bar says so once, not for every try. `client.forward(8080)` in your config forwards a port whenever the window opens.

A download through a forward goes as background traffic: only as fast as the link takes it without a queue building up, so what you type and the screens it changes never wait behind it, however slow the link. On a link that's slower whenever it's busy, as some Wi-Fi is, that can leave a download crawling: `:set noforwardpace` lets forwards take the whole link instead, at the cost of keys waiting behind them, for connections made after.

## From your phone

The daemon can serve a web app, for your phone or any browser: your chats by workspace, with those that need you first, and each chat as a window shows it, with its questions, the box to send a message, Stop, the model, mode and effort menus, and the cards of the chats its agent started. It's off until `~/.config/clackatoo/daemon/web.toml` says `mode = "tailscale"`, which listens on a socket only you can reach, `~/.local/share/clackatoo/web/web.sock`, for `tailscale serve --bg unix:` and that path to give your tailnet, over HTTPS; `mode = "public"` serves it to the network, with your own certificate and a password.

A phone pairs once: with the password `clackatoo web password` sets, or by asking a window. Asking shows a code on the phone, and the window's bar says a device asks; type `:Devices allow` and the code to let it in, or `:Devices deny`. `:Devices` lists what's paired, and `:Devices revoke 2` unpairs the second, cutting it off at once. After fifty wrong passwords in a row, pairing with the password locks, the bar says so, and `:Devices unlock` lets it again. Turn on notifications in the app, and the phone is told when a chat finishes, or asks you something, while no window you're using shows it. On an iPhone, add the app to the home screen first (Share, then Add to Home Screen), and pair it from there: only an app on the home screen gets notifications.

## Terminals from your phone

The app lists each workspace's terminals under its chats, with what each is running and where. Tap one to see it as it is, at its own size: its rows as wide as the phone allows, and if they're wider still, panned sideways with a finger; pinch to zoom. Scroll up for its history. **Fit** sizes the terminal to the phone instead, and says Fitted while it lasts; it goes back to the size your windows want when you tap Fitted, leave the terminal, or type into it from a window.

Tap the rows to bring up the keyboard; what you type goes as you type it, a key at a time, and the row above the keyboard has Esc, Tab, Ctrl (for the next key), Ctrl+C, the arrows, `|`, `~`, `/`, `-` and Paste. When the daemon can't be reached, or hasn't answered for three seconds, keys are turned down and the phone says so, rather than reach a shell long after you meant them.

## Another machine's workspaces, in your bar

`:connect HOST` has your daemon connect to the daemon on HOST, through ssh as `--remote` does, and bring HOST's workspaces into your bar beside your own. Their terminals and chats work as your own do: what you type goes there and their screens come back, through your daemon, so your phone and your scripts have them too. `:Terminal`, a split, a rename or closing a tab in one of them happens on HOST, which keeps them, so they carry on while your laptop sleeps. Editors, explorers, the finder and boards on HOST come in a later release: until then, their panes say so.

A workspace from another machine has a ring in place of its dot: in its colour while it's connected, red while the connection is down and being made again, and amber when ssh needs a password no window has given yet, which `:connect HOST` asks for again. The mouse over it says which machine it's on, and how its connection is. If the connection drops, its workspaces stay in the bar as they last were, each pane saying so over its last screen, and keys for its terminals are turned down until it's back. Your daemon keeps the connection, makes it again whenever it drops, and after it restarts; `:disconnect HOST`, or `:disconnect` in one of its workspaces, ends it.

A workspace's name says which machine it's on only while another has the same name, as `notes@desk`. `:set showhost=always` says it for every one, and `never` for none. Try it:

```check
option showhost=always
```

`:workspace notes@desk` switches to a workspace by its machine. Tab after `:connect` offers the machines in your `~/.ssh/config` and those you've connected to lately. In your config, `client.connect("desk")` connects whenever a window opens, saying nothing unless it fails.

`:connect` alone still opens a window on another machine, and `:Terminal@host`, for a pane on another machine beside your own, comes in a later release.
