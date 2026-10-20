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

## Your desktop's display

The daemon on your desktop was most likely started over ssh, before you'd logged in to the desktop itself, yet what it starts still opens its windows there. As you log in, your desktop tells systemd its display (`WAYLAND_DISPLAY`, `DISPLAY` and the like), and the daemon gives it to every terminal, agent and language server it starts from then on. Shells already open take it at their next prompt, but for a variable you've set yourself in that shell, which stays yours; programs already running keep what they started with, as on any Unix. Once you log out, what starts gets no display again. A desktop that tells systemd nothing, as sway started by hand, still works for a daemon started from inside it.

`clackatoo display`, on the desktop, says what programs get now and where it came from: `systemd` while you're logged in, `inherited` for the daemon's own, or `none`. `CLACKATOO_DISPLAY=off` where the daemon starts leaves everything the daemon's own environment, as before.

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

`:connect HOST` has your daemon connect to the daemon on HOST, through ssh as `--remote` does, and bring HOST's workspaces into your bar beside your own. Their terminals and chats work as your own do: what you type goes there and their screens come back, through your daemon, so your phone and your scripts have them too. `:Terminal`, a split, a rename or closing a tab in one of them happens on HOST, which keeps them, so they carry on while your laptop sleeps.

Everything else in them works as your own does, on HOST: editors, with undo, `:w`, `:w NAME` for a buffer with no name yet, and `:recover`; their colours, git's signs and what agents changed; what HOST's language servers say, with `K`, completion, `gd`, `grn`, `gra`, symbols, inlay hints and `:Format`; explorers and folders' listings, whose `:w` moves and deletes HOST's files; reviews, boards and a chat's browser. The finder searches the machine the focused pane is on, so `<Space>ff` in one of HOST's workspaces finds HOST's files, and greps them there. HOST keeps each file and makes every edit, so its own windows, and its agents, edit a file with you at once as a window and an agent here do; what you type while the connection is down is made once it's back. HOST's clackatoo must be of this release or later for all of this: an older one's editors and explorers say so in their panes, and its terminals and chats still work.

`:PortForward 8080` in one of HOST's workspaces forwards this machine's port 8080 to HOST's, through your daemon's connection to it, which keeps listening while the connection drops and is made again. `:PortForward` there lists them, and `:PortForward! 8080` stops one, as for a window on another machine; a window that's itself on another machine than its daemon can't forward to HOST yet.

A workspace from another machine has a ring in place of its dot: in its colour while it's connected, red while the connection is down and being made again, and amber when ssh needs a password no window has given yet, which `:connect HOST` asks for again. The mouse over it says which machine it's on, and how its connection is. If the connection drops, its workspaces stay in the bar as they last were, each pane saying so over its last screen, and keys for its terminals are turned down until it's back. Your daemon keeps the connection, makes it again whenever it drops, and after it restarts; `:disconnect HOST`, or `:disconnect` in one of its workspaces, ends it.

A workspace's name says which machine it's on only while another has the same name, as `notes@desk`. `:set showhost=always` says it for every one, and `never` for none. Try it:

```check
option showhost=always
```

`:workspace notes@desk` switches to a workspace by its machine. Tab after `:connect` offers the machines in your `~/.ssh/config` and those you've connected to lately. In your config, `client.connect("desk")` connects whenever a window opens, saying nothing unless it fails.

`:connect` alone still opens a window on another machine.

## Machines your machines are connected to

Your daemon passes on the machines it's connected to. If your laptop is connected to desk, and desk to a server, the laptop's bar has the server's workspaces too, as `notes@server`: named after the machine they live on, never the way there. They work as any machine's do, editors, chats, the finder and `@server` panes included, through a connection of the laptop's own to the server that goes through desk. Nothing is shown twice. Desk could read or change what it carries, so your laptop checks the server is the server, by the key it knows it by: from when it last reached it with ssh, or, for a server it has never reached itself, the key desk vouches for the first time, kept from then on. A server whose clackatoo chose a new key, as when its `machine` file was deleted, is refused, saying so, until `clackatoo forget server`. The mouse over such a workspace says the way, as `on server, through desk`.

A machine reached more than one way is shown once, by the shortest. `:connect server` on the laptop too has it go straight there, and through desk only while ssh can't get there, moving back once it can, all without its panes noticing. `:disconnect server` then leaves it reached through desk; one only desk passes on goes when desk stops passing it on, or with `:disconnect desk`. Machines connected in a loop, as a laptop and desk connected to each other, each show the others once, and never themselves.

A server you share with others needn't be passed on: `:connect server --no-share` keeps it from the daemons connected to yours, and refuses them a way through; `:connect server --share` passes it on again. Your daemon keeps the setting with the connection. In your config, `client.connect("server", {share = false})` does the same.

## A pane on another machine, beside your own

A new pane goes on the machine of the pane you make it from: `:Terminal`, a split, `:e`, `:Explore` or the finder in one of desk's panes is desk's, wherever its workspace is. `@host` after a kind of pane picks the machine instead: `:Terminal@desk` opens a terminal on desk in a new tab of the workspace you're in, `:|Terminal@desk` splits one in beside the pane you're in, and `:Editor@desk notes.md`, `:Explore@desk` and the rest work the same. `@local`, or this machine's own name, is your daemon's machine. Tab after the `@` offers the machines your daemon is connected to.

Such a pane is desk's in every way: what you type runs there, and a split or a file opened from it is desk's too. Its tab says which machine it's on, as `2 src@desk`, unless `showhost` is `never`. Your workspace keeps it, after your daemon restarts too, and desk keeps what it shows, after desk restarts too: a terminal comes back as a new shell in its folder there, as desk's own do. While desk can't be reached the pane says so over its last screen, and closing it closes it on desk once desk is back. Desk never throws away what it lent on so little: a pane nothing here shows any more, or a `[No Name]` with changes, comes back as one of desk's own workspaces, `from-laptop`, unless it's a shell running nothing. On desk, `clackatoo lent` lists what it lent, and `clackatoo lent reclaim N` takes one back; after a month without your laptop, desk takes them all back.

## A pane of this machine's in desk's workspace

It works the other way round too: `:|Terminal@local` in one of desk's workspaces splits in a terminal on this machine, and any pane made beside it is this machine's as well. Desk's layout keeps it, while the terminal itself stays here: what you type goes straight to it, never to desk and back. Desk can't reach your laptop, so desk's own windows, and its phone, show "a terminal on laptop, not reachable from here" in its place; a machine connected to both shows it, and so does desk once `:connect laptop` there.

While desk is away, such panes are parked in a workspace of this machine's named after it, `from-desk`, a tab each, so you can go on using them; once desk is back they're back where they were, and that workspace goes. Closing `from-desk` closes them all; new panes beside them wait until they're back in desk's workspace. Closing one here closes it for good. Closed on desk, a shell running nothing closes, and anything else comes back as one of this machine's own workspaces, `from-desk`, so nothing is lost; one desk restarted without always comes back so.

`:workspace new@desk notes` makes a workspace called notes on desk, and `:workspace notes@desk` does too if desk has none of that name. The `+` at the end of the workspaces asks which machine once your daemon is connected to another. A workspace made on desk lives there, so it carries on while your laptop sleeps, and desk's own windows have it too.
