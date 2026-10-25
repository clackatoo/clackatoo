---
title: Your phone
type: reference
description: "The web app the daemon serves, for your phone or any browser: setting it up over Tailscale or the network, pairing devices, notifications, and what's kept where."
related:
  - clackatoo/reference/remote
  - clackatoo/reference/config
---

The daemon can serve a web app: your chats by workspace, with those that need you first, and each chat as a window shows it, with its questions, the message box, Stop, the model, mode and effort menus, how much context it fills, and the cards of the chats its agent started. A phone can answer anything a window can, tool approvals included. And your terminals, to read and type into.

## Turning it on

It's off until `~/.config/clackatoo/daemon/web.toml` says otherwise. The daemon reads it when it starts or upgrades (`clackatoo upgrade`).

    mode = "tailscale"   # or "public", or "off"
    login = "you@github" # tailscale: who's let in
    origin = "https://desktop.example.ts.net"
    expire_days = 90     # unpair devices unused this long
    password_days = 0    # ask for the password again after this many days
    ask = true           # devices may ask a window to let them in
    lock_after = 50      # wrong passwords in a row before password pairing locks

### Through Tailscale

The daemon listens on a socket only you can open, `~/.local/share/clackatoo/web/web.sock`. Give it to your tailnet over HTTPS with

    tailscale serve --bg unix:$HOME/.local/share/clackatoo/web/web.sock

and open `https://<machine>.<tailnet>.ts.net`. A request must come through `tailscale serve`, carrying Tailscale's `Tailscale-User-Login`, and that login must be `login`. Without `login` set, only a window can let the first device in. The socket keeps other users of the machine out, but not your own programs: what lets a device in is the token it's given when it pairs.

### Straight from the network

    mode = "public"
    address = "0.0.0.0"
    port = 8765
    certificate = "/path/fullchain.pem"
    key = "/path/privkey.pem"
    origin = "https://example.com:8765"

This needs a password and `origin`, and `ask` is off unless you turn it on. Getting certificates by ACME isn't built yet, so you bring your own; it's read again when its files change.

## Pairing a device

A device pairs once:

- **with the password**, which `clackatoo web password` sets. Only its Argon2id hash is kept, in web.toml. Tries are limited to five at once, then one a minute, from each address (IPv6 by /64), and to about twenty a minute from everywhere together. After `lock_after` wrong passwords in a row, password pairing locks, and stays locked after a restart, until `:devices unlock` or `clackatoo devices unlock`; every window's bar says so. Anyone can lock it this way, but devices already paired keep working.
- **by asking a window.** The phone shows a code, and every window's bar says a device asks. Type `:devices allow CODE` to let it in, or `:devices deny`. The window never shows the code, so only someone holding the phone can let it in.

The device then keeps a token in a cookie its scripts can't read; the daemon keeps only its hash. `:devices` (or `clackatoo devices`) lists paired devices, and `:devices revoke N` unpairs one, cutting it off at once. A device unused for `expire_days` is unpaired, and one past `password_days` is asked for the password again; either cuts off a device that's connected.

On an iPhone, add the app to the home screen first (Share → Add to Home Screen) and pair it from there: the home-screen app keeps its own cookies, and only it can get notifications.

What a phone does in a chat is logged with the device's number.

While the agent works, the bar at the foot of a chat says what it's doing and for how long, beside three pulsing dots, however far up you've scrolled. The seconds count only while the page is on screen and the app is connected, and the dots keep still if your phone asks for less motion. In the list of chats, running agents have a blue activity ring; approvals, unread replies and failures have distinct markers.

## Managing agents

The overview opens on **Agents**, grouped by workspace. **Needs you** filters to approvals, unread replies and failed sessions; **Working** shows running agents. Workspaces that need attention come first. Search matches chat titles, providers and workspace names within the selected filter. **Terminals** is a separate view, with search matching names and directories. The top bar says whether the app is connected or retrying.

Long chats open on their recent turns first. Scrolling near the top fetches older messages without moving the message you're reading; **Load earlier messages** also fetches a page. Each connected client keeps its own loaded range, and reconnecting retains it. The daemon keeps the complete history.

In a chat, the conversation scrolls independently of its header and composer. New output follows while you're near the bottom; scrolling back keeps your place and reveals **Latest** to return to the end. Pending approvals stay above the composer, with their full choices. On wide screens, the session list stays beside the conversation. The composer follows the phone's visible screen when the keyboard opens and expands back when it closes. It sits close to the bottom, reserving the home-indicator area only while that area is visible. Long tool output and code scroll sideways inside their own blocks. Message attachments and tool images show their retained thumbnails.

Keyboard positioning also updates when you tap an already-focused message box or return to the app. Safari can report its new visible size late in the keyboard animation; the app briefly rechecks the size after these transitions without changing your place in older messages.

Tap the model name for **Agent settings**: model, permissions, reasoning and any other settings offered by the provider. Search models across all providers, or choose a provider tab. Star a model to keep it in Favorites, saved in this browser. Search shows matching favorites first; clearing it returns to the previous provider. The sheet opens on Favorites when you have any, otherwise on the current provider. The shield beside the model opens permissions, tools and browser access together. The context gauge opens **Context & usage**, with the chat's token counts and available account allowances across providers. Opening it asks for fresh readings while showing cached values. Both panels close with their close button, a tap outside, or Escape; keyboard focus stays inside the panel until it closes.

**Send** submits a message when idle. While the agent is working, **Queue** adds it to the shared queue. **Steer now** sends text into supported active sessions without stopping their work; unsupported sessions explicitly say **Stop & continue**. Ctrl+Enter (Command+Enter on a Mac) sends when idle and steers while working; plain Enter adds a line. Queued messages have controls to send them now or remove them. **Stop** always stops the agent.

Draft text stays with its chat when you navigate away, and survives reloads in the same browser tab through session storage. It clears only after the daemon accepts that exact message. If sending fails or the connection is lost, the text is kept. A lost acknowledgment may mean the message was delivered: the app tells you to check the conversation before retrying, and never resends it automatically. Drafts aren't shared between devices; an unavailable browser storage API limits recovery to the current page.

## Terminals

The **Terminals** filter lists each workspace’s terminals, with what each is running, where, and its size; terminals in no workspace are under Elsewhere. Tap one to follow it.

A terminal is shown at its own size, the one your windows gave it: as wide as the phone allows, but never in type under 7 pixels, so a wider one pans sideways under your finger. Pinch to zoom. Scroll up for its history, fetched as you go. A command that failed ends its prompt's line with a dim red `✗` and its exit status, as in a window, on the screen and in history.

**Fit** makes the terminal the phone's size instead, and says Fitted while it lasts; it's fitted again when the keyboard comes up or goes. It goes back when you tap Fitted, leave the terminal or put the phone away, or type into it from a window; windows' resizes wait till then, and it takes the latest they asked for. A phone that stops asking for the fit, as one asleep does, loses it within a minute. The phone never resizes a terminal any other way.

Tap the rows for the keyboard. What you type goes as you type it, a key at a time, with the keyboard's corrections off and curly quotes made straight. Two rows above the keyboard have Esc, Tab, Ctrl (for the next key: Ctrl then c is Ctrl+C), ^C, the arrows, `|`, `~`, `/`, `-` and Paste; a keyboard attached to the phone works too. A paste of several lines into a program that hasn't asked for pastes marked as such asks first, since each line would run as you'd typed it; a paste over a megabyte is turned down. When the daemon can't be reached, or hasn't answered for three seconds, keys are turned down and the phone says how many, rather than reach a shell long after you meant them. Nothing is kept to send later.

The log notes the device that fits a terminal, or starts typing into one, never what it typed.

## Notifications

Tap **Enable** in the overview. The phone is then told when a chat finishes a turn, or asks you something, while no window you're using shows it. A window counts as in use while it has the focus and has been used in the last five minutes. A notification carries only the chat's title and a few words, sealed for your device, and goes only to Apple's, Google's, Mozilla's or Microsoft's push services.

## What's kept

`~/.local/share/clackatoo/web/`: `devices.json`, with names, token hashes, push subscriptions and whether password pairing is locked, and `vapid.p8`, the key notifications are signed with. Only you can read either.

## A chat keeps reconnecting

If a chat stays on **Opening** while the connection keeps retrying, inspect the daemon's `daemon.log` in its runtime directory. Repeated "dropping a client on another machine" warnings with more than 8 MiB pending can indicate an older daemon whose send-buffer limit is smaller than a valid conversation snapshot. Update the daemon; a large initial snapshot is allowed to drain before further view updates are sent.

## Building it

The web app is built into the program when it's compiled, which needs the `wasm32-unknown-unknown` target and `wasm-bindgen-cli` 0.2.129. A build without them serves a page saying the app is missing; `CLACKATOO_WEB_APP=off` leaves it out on purpose. Releases always include it.
