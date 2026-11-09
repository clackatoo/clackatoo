---
title: Your Phone
type: reference
description: "The web app the daemon serves, for your phone or any browser: setting it up over Tailscale or the network, pairing devices, notifications, and what's kept where."
related:
  - clackatoo/reference/remote
  - clackatoo/reference/config
---

The daemon can serve a web app: your chats by workspace, with those that need you first, and each chat as a window shows it, with its questions, the message box, Stop, the model, mode and effort menus, how much context it fills, and the cards of the chats its agent started. A phone can answer anything a window can, tool approvals included. And your terminals, to read and type into.

## Turning it on

It's off until `~/.config/clackatoo/daemon/web.toml` says otherwise. The daemon reads it when it starts or upgrades (`clackatoo upgrade`), and logs any key in it that isn't a setting.

```toml
mode = "tailscale"   # or "public", or "off"
login = "you@github" # tailscale: who's let in
origin = "https://desktop.example.ts.net"
expire_days = 90     # unpair devices unused this long
password_days = 0    # ask for the password again after this many days
ask = true           # devices may ask a window to let them in
lock_after = 50      # wrong passwords in a row before password pairing locks (1 to 10000)
```

### Through Tailscale

The daemon listens on a socket only you can open, `~/.local/share/clackatoo/web/web.sock`, or where `socket` says. Give it to your tailnet over HTTPS with

```sh
tailscale serve --bg unix:$HOME/.local/share/clackatoo/web/web.sock
```

and open `https://<machine>.<tailnet>.ts.net`. A request must come through `tailscale serve`, carrying Tailscale's `Tailscale-User-Login`, and that name must be `login`. Without `login` set, only a window can let the first device in. The socket keeps other users of the machine out, but not your own programs: what lets a device in is the token it's given when it pairs. Connections are counted by the device sending each request, by its whole tailnet address (IPv6 too), as `tailscale serve` says: each can have eight open at once, and its password tries and asks to pair are its own. At most sixteen connections can wait to say whom they're from; past that, the oldest is closed. Past 64 in all, room is made as from the network (below).

### Straight from the network

```toml
mode = "public"
address = "0.0.0.0"
port = 8765
certificate = "/path/fullchain.pem"
key = "/path/privkey.pem"
origin = "https://example.com:8765"
```

This needs a password and `origin`, and `ask` is off unless you turn it on. clackatoo doesn't get certificates by ACME, so you bring your own; it's read again when its files change. Prefer an ECDSA key, as certbot, acme.sh, and `tailscale cert` make by default: each new connection's TLS handshake is worked out by the daemon itself, and with an RSA key one costs about ten times as much.

Anyone on the network can open connections, so the daemon limits them. One address can open twenty in a burst, then five a second, and keep eight open at once; IPv6 addresses count by their /64. At most 64 are open in all, and connections that haven't shown a paired device's token can hold only 56 of them: the rest are kept for addresses a paired device's token came from in the last hour. When room is wanted, the one open longest of those that haven't shown a token is closed, whatever it's doing; one that has is never closed while any that hasn't is open. So however many connections others open, from however many addresses, a paired device gets in from where it was last used within the hour. From anywhere else, its new connection is a stranger's until its first request shows its token: it gets in, but a flood of new connections can close it before then. Only when every connection has shown one can a device's connection close another device's: one open over a second, waiting on nothing, and idle or reading what it was sent slower than 4 KiB a second for three seconds. Only four handshakes are worked on in each turn of the daemon's loop, the rest waiting, in order, for the next, so a flood of them slows new connections, not your terminals. Connections a device's app keeps open to follow your chats don't count.

## Pairing a device

A device pairs once:

- **with the password**, which `clackatoo web password` sets. Only its Argon2id hash is kept, in web.toml. Tries are limited to five at once, then one a minute, from each address (IPv6 by /64 from the network, and by the whole address through Tailscale), and to about twenty a minute from everywhere together. After `lock_after` wrong passwords in a row (50 unless set), password pairing locks, and stays locked after a restart, until `:devices unlock` or `clackatoo devices unlock`; every window's bar says so. Anyone who can reach the app can lock it this way, in about a minute and a half, and in public mode keep it locked; devices already paired keep working, and a window can still allow one by its code.
- **by asking a window.** The phone shows a code, and every window's bar says a device asks. Type `:devices allow CODE` to let it in, or `:devices deny`. The window never shows the code, so only someone holding the phone can let it in. At most four devices ask at once, each address four times in two minutes, so whoever can reach the app can fill your bar with asks and names of their choosing, though never get in by them.

The device then keeps a token in a cookie its scripts can't read; the daemon keeps only its hash. Every window's bar says so whenever a device pairs, however it did, such as `phone (you@github) paired with the password: :devices revoke 3 unpairs it`. Windows go on being told until one has shown it: one paired while no window is open is said by the next window to open, even after the daemon restarts, as devices.json keeps it. Only windows are told, not devices or the CLI's commands. `:devices` (or `clackatoo devices`) lists paired devices, and `:devices revoke N` unpairs one, cutting it off at once. Devices are listed and unpaired only from a window or `clackatoo devices`; a device can unpair only itself. A device unused for `expire_days` is unpaired, and one past `password_days` is asked for the password again; either cuts off a device that's connected. A device counts as used when it connects, so one that stays connected longer than `expire_days` is unpaired while in use. One that's to give the password again gets no notifications until it does.

On an iPhone, add the app to the home screen first (Share → Add to Home Screen) and pair it from there: the home-screen app keeps its own cookies, and only it can get notifications.

What a phone does in a chat is logged with the device's number.

### A paired device has a shell

A paired device can do almost anything a window can, and it can type into your terminals. The daemon refuses a few things to devices: connecting to other machines or forgetting them, `:keep`, allowing other devices to pair, unlocking pairing, and listing or unpairing devices. Those are guards against mistakes, not a boundary: from a terminal, a device can run `clackatoo` itself, or anything else you could. A lost phone that's still paired is a lost shell on your computer. Unpair it with `:devices revoke N` at once, then check what's paired with `:devices`: every pairing since was said in your windows' bars.

While the agent works, the bar at the foot of a chat says what it's doing and for how long, beside three pulsing dots, however far up you've scrolled. The seconds count only while the page is on screen and the app is connected, and the dots keep still if your phone asks for less motion. In the list of chats, running agents have a blue activity ring; approvals, unread replies, and failures have distinct markers.

## Managing agents

The overview opens on **Agents**, grouped by workspace. **Needs you** filters to approvals, unread replies, and failed sessions; **Working** shows running agents. Workspaces that need attention come first. Search matches chat titles, providers, and workspace names within the selected filter. **Terminals** is a separate view, with search matching names and directories. The top bar says whether the app is connected or retrying.

Long chats open on their recent turns first. Scrolling near the top fetches older messages without moving the message you're reading; **Load earlier messages** also fetches a page. Each connected client keeps its own loaded range, and reconnecting retains it. The daemon keeps the complete history.

In a chat, the conversation scrolls independently of its header and composer. New output follows while you're near the bottom; scrolling back keeps your place and reveals **Latest** to return to the end. Pending approvals stay above the composer, with their full choices. On wide screens, the session list stays beside the conversation. The composer follows the phone's visible screen when the keyboard opens and expands back when it closes. It sits close to the bottom, reserving the home-indicator area only while that area is visible. Long tool output and code scroll sideways inside their own blocks. Message attachments and tool images show their retained thumbnails.

Links in an agent's words open in a new tab; only web and mail links do, and a link's text, written by the agent, might not be where it goes. Press and hold one to see its address. A link an agent asks you to open in a form always shows its address.

Keyboard positioning also updates when you select an already-focused message box or return to the app. Safari can report its new visible size late in the keyboard animation, with no event saying so; for a second and a half after each of these transitions the app reads the size on every frame and follows it, without changing your place in older messages. An idle page reads nothing. Home Screen mode and pinch zoom also stay within the browser's reported visible area; CSS viewport units can extend beyond the drawable window on iOS. The app lets iOS choose its status-bar appearance instead of requesting the deprecated translucent mode. The app's bottom padding is capped at 12 pixels with the keyboard closed and is 6 pixels with it open; space reserved outside the page by iOS is separate.

If the composer still ends up behind the keyboard, or a gap shows under it, select **Screen measurements** at the foot of the list (touch screens only). It shows, over the page, the sizes the phone reports: its visual viewport, the layout viewport, the window's scroll, the CSS viewport units and safe-area insets, where the page and the composer end, and the latest keyboard, focus and scroll events with their timings. A screenshot of it tells what the phone said when the layout went wrong. It's kept on in that browser (or Home Screen app) until you select the button again, and never takes a touch itself.

Select the model name for **Agent settings**: model, permissions, reasoning, and any other settings offered by the provider. Search models across all providers, or choose a provider tab. Star a model to keep it in Favorites, saved in this browser. Search shows matching favorites first; clearing it returns to the previous provider. The sheet opens on Favorites when you have any, otherwise on the current provider. Picking another of the chat's own agent's models changes it in the chat. Picking another provider's model moves the chat to that agent at once, though not while the agent answers; unlike in a window, it isn't held for your next message. Your next message takes the new agent up to about 20,000 tokens of the conversation so far, or only what happened since if the chat had that agent before. The same sheet has **Permissions** for approval mode, tool policy, browser access and how many subagents can work, and **Tools** for the live harness capability report. These controls share the model button in the composer. The context gauge opens **Context & usage**, with the chat's token counts and available account allowances across providers. Opening it asks for fresh readings while showing cached values. Both panels close with their close button, selecting outside them, or Escape; keyboard focus stays inside the panel until it closes.

**↑** sends a message when idle. While the agent is working, it says **Queue** and adds the message to the shared queue, and a **Send to queue** row above the box offers **Steer now**, which sends the text into supported active sessions without stopping their work. Sessions that can't be steered, and any message with photos, say **Stop & continue** instead; a message that might mention files says **Stop & send**, since a file it mentions goes only with the message whole. Ctrl+Enter (Command+Enter on a Mac) sends when idle and steers while the agent answers; plain Enter adds a line. A message that is only the name of a command the agent's own interface has, such as `/model` or `/clear`, isn't sent: the app says to use it in a window. Queued messages are listed above the box under **Up next** (or **Queue paused**), each with a button to send it now (steering, or **Stop & continue**, while the agent works) and × to remove it; there's no pencil to take one back to edit, as a window has. **Stop**, in the chat's header while the agent answers, always stops it.

Draft text stays with its chat when you navigate away, and survives reloads in the same browser tab through session storage. It clears only after the daemon accepts that exact message. If sending fails or the connection is lost, the text is kept. A lost acknowledgment might mean the message was delivered: the app tells you to check the conversation before retrying, and never resends it automatically. The daemon keeps each chat's draft too, as it does for a window's box: the app tells it what's typed after a short pause in typing and when you leave the chat, and a chat opened in this page for the first time starts with the daemon's draft unless you've typed in its box since. When a message is sent from any box, the app's box empties if it holds only that message. An unavailable browser storage API limits recovery after a reload to what the daemon kept.

A daemon that starts afresh, as after the computer restarts (an upgrade doesn't), numbers its chats and terminals anew, so chat 7 might be another chat. When the app reconnects to such a daemon, it closes the chat or terminal it showed, goes back to the list and says why, and drops the drafts and photos it held, so nothing typed for one chat is sent to another or typed into another terminal. Each time it connects, the app sends no message, key, draft or fit until the daemon has said which run it is; a message tried meanwhile says it's still connecting, and keys are turned down. Drafts kept in the tab's storage are kept for one run of the daemon only: a reload shows the chat's draft at once, and drops it if the app then finds the daemon started afresh.

**＋** (Attach photos), beside the send button, opens the device's photo/file picker, including in the iPhone Home Screen app. Choose up to four photos or screenshots, each under 20 MiB, and review the thumbnails above the message box; × removes one. You can send photos with or without text, or queue them while an agent works. Photos are prepared locally and uploaded only when you send. Browser-decodable images are converted to PNG, with a maximum side of 4,096 pixels and the same 8 MiB encoded message limit as the native client. An unsupported format asks you to choose a PNG or JPEG export.

Unsent photos stay with their chat across navigation and connection interruptions, and clear only when their submission is accepted. They live in this page's memory, at most 32 MiB of them across all chats, so reloading or closing the page discards them; text drafts still survive reloads. An unconfirmed send never retries automatically.

## Terminals

The **Terminals** filter lists each workspace's terminals, with what each is running, where, and its size; terminals in no workspace are under Elsewhere. Select one to follow it.

A workspace's [background tabs](~clackatoo/reference/window#background-tabs) are listed after its terminals, those of another machine's workspaces your daemon shows too: each with how it's getting on (Running, Idle, Ended, or Failed with its exit status), the ports its programs listen on, and ` ↻` if it's kept running. Select one to follow its terminal; **Bring back** puts it back in its workspace's tab bar and shows it; **✕** dismisses it, closing its terminals, and one still running asks you to select it again, **Stop?**, within three seconds. A workspace's heading says how many of its background tabs are running, `⚙ 2`, red with the exit status of one that failed, `⚙ 1 ✗ 2`, as a window's icon after the tabs does; select it to list them. Listing them counts as having seen a failure, as a window's list does, but the list goes on saying it while it's shown. The app doesn't mark a terminal to keep running (the daemon refuses that from a phone: it's for you at a window), nor offer starting a background tab.

A terminal is shown at its own size, the one your windows gave it: with readable 14–16 pixel text by default, so a wider one pans sideways under your finger. Pinch to zoom, or open **⋯** for text size controls and Reset. These change only this browser's view. Scroll up for its history, fetched as you go. A command that failed ends its prompt's line with a dim red `✗` and its exit status, as in a window, on the screen and in history.

**Fit**, in **⋯**, makes the terminal the phone's size instead, and says Fitted while it lasts; it's fitted again when the keyboard comes up or goes. It goes back when you select Fitted, leave the terminal or put the phone away, or type into it from a window; windows' resizes wait till then, and it takes the latest they asked for. A phone that stops asking for the fit, as one asleep does, loses it within a minute. The phone never resizes a terminal any other way.

Select the rows or **Keyboard** to type; the down chevron hides the keyboard again. What you type goes as you type it, a key at a time, with the keyboard's corrections off and curly quotes made straight. One compact row appears while typing, with Esc, Tab, Ctrl (for the next key: Ctrl then c is Ctrl+C) and the arrows. **⋯** holds ^C, `|`, `~`, `/`, `-`, and Paste; using these controls keeps the input focused. A keyboard attached to the phone works too. A paste of several lines into a program that hasn't asked for pastes marked as such asks first, since each line would run as you'd typed it; a paste over a megabyte is turned down. When the daemon can't be reached, or hasn't answered for three seconds, keys are turned down and the phone says how many, rather than reach a shell long after you meant them. Nothing is kept to send later.

The log notes the device that fits a terminal, or starts typing into one, never what it typed.

## Notifications

Select **Enable** in the overview. The phone is then told when a chat finishes a turn, or asks you something, while no window you're using shows it. A window counts as in use while it has the focus and has been used in the last five minutes. A notification carries only the chat's title and a few words, sealed for your device, and goes only to Apple's, Google's, Mozilla's or Microsoft's push services. A window on a desktop shows its own [desktop notifications](~clackatoo/reference/agents#desktop-notifications) besides, on the computer it runs on; one without the focus says nothing of a turn you watched end on the phone.

## What's kept

`~/.local/share/clackatoo/web/`: `devices.json`, with names, token hashes, push subscriptions, pairings no window has shown yet, and whether password pairing is locked, and `vapid.p8`, the key notifications are signed with. Only you can read either.

## A chat keeps reconnecting

If a chat stays on **Opening** while the connection keeps retrying, inspect the daemon's `daemon.log` in its runtime directory. Repeated "dropping a client on another machine" warnings with more than 8 MiB pending can indicate an older daemon whose send-buffer limit is smaller than a valid conversation snapshot. Update the daemon; a large initial snapshot is allowed to drain before further view updates are sent.

## Building it

The web app is built into the program when it's compiled, which needs the `wasm32-unknown-unknown` target and `wasm-bindgen-cli` 0.2.129. A build without them serves a page saying the app is missing; `CLACKATOO_WEB_APP=off` leaves it out on purpose, and `CLACKATOO_WEB_APP=required` makes a build without them fail, as releases' builds do, so releases always include it. `CLACKATOO_WASM_BINDGEN` names the wasm-bindgen program, if it isn't `wasm-bindgen` on `PATH`.
