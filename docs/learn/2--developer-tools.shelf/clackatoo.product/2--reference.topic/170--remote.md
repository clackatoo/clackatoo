---
title: Another Machine
type: reference
description: With --remote, the window and the clackatoo command use the daemon on another machine, reached with ssh. Which page covers connecting and lending panes, how a connection works and recovers, and compatibility between builds.
related:
  - clackatoo/reference/phone
  - clackatoo/reference/window
  - clackatoo/reference/cli
---

The window and the `clackatoo` command can use the daemon on another machine
instead of this one: your desktop from your laptop, or a server. The terminals
run there, and keep running when you disconnect.

```sh
clackatoo --remote desktop                  # a window on desktop's workspaces
clackatoo list --remote desktop             # its terminals
clackatoo new --remote jack@server -- htop  # start one there
```

The host is anything `ssh` accepts: a hostname, `user@host`, `host:port`, or a
name from your `~/.ssh/config`, where keys and jump hosts belong.

These pages cover using another machine, each listed with its sections:

- [Connecting to Another Machine](~clackatoo/reference/connecting):
  <span id="picking-a-machine"></span>[Picking a machine](~clackatoo/reference/connecting#picking-a-machine),
  <span id="another-machine-s-workspaces-in-your-bar"></span>[Another machine's workspaces in your bar](~clackatoo/reference/connecting#another-machine-s-workspaces-in-your-bar),
  <span id="upgrading-your-daemon"></span>[Upgrading your daemon](~clackatoo/reference/connecting#upgrading-your-daemon),
  <span id="machines-passed-on"></span>[Machines passed on](~clackatoo/reference/connecting#machines-passed-on),
  <span id="a-pane-on-another-machine"></span>[A pane on another machine](~clackatoo/reference/connecting#a-pane-on-another-machine),
  <span id="forwarding-ports"></span>[Forwarding ports](~clackatoo/reference/connecting#forwarding-ports),
  <span id="from-your-phone"></span>[From your phone](~clackatoo/reference/connecting#from-your-phone).
- [How a Connection Works](~clackatoo/reference/connections):
  <span id="how-it-connects"></span>[How it connects](~clackatoo/reference/connections#how-it-connects),
  <span id="moving-onto-udp"></span>[Moving onto UDP](~clackatoo/reference/connections#moving-onto-udp),
  <span id="firewalls"></span>[Firewalls](~clackatoo/reference/connections#firewalls),
  <span id="what-the-other-machine-needs"></span>[What the other machine needs](~clackatoo/reference/connections#what-the-other-machine-needs),
  <span id="the-other-machine-s-desktop"></span>[The other machine's desktop](~clackatoo/reference/connections#the-other-machine-s-desktop),
  <span id="an-image-from-this-machine-s-clipboard"></span>[An image from this machine's clipboard](~clackatoo/reference/connections#an-image-from-this-machine-s-clipboard),
  <span id="options"></span>[Options](~clackatoo/reference/connections#options),
  <span id="when-the-other-machine-stops-answering"></span>[When the other machine stops answering](~clackatoo/reference/connections#when-the-other-machine-stops-answering),
  <span id="when-ssh-s-connection-closes"></span>[When ssh's connection closes](~clackatoo/reference/connections#when-ssh-s-connection-closes),
  <span id="when-the-connection-drops"></span>[When the connection drops](~clackatoo/reference/connections#when-the-connection-drops),
  <span id="bad-networks"></span>[Bad networks](~clackatoo/reference/connections#bad-networks),
  <span id="logs"></span>[Logs](~clackatoo/reference/connections#logs),
  <span id="two-windows-on-one-terminal"></span>[Two windows on one terminal](~clackatoo/reference/connections#two-windows-on-one-terminal),
  <span id="typing-over-a-slow-connection"></span>[Typing over a slow connection](~clackatoo/reference/connections#typing-over-a-slow-connection).
- [Compatibility between Builds](~clackatoo/reference/compatibility):
  <span id="protocol-compatibility"></span>[Protocol compatibility](~clackatoo/reference/compatibility#protocol-compatibility),
  <span id="a-daemon-from-another-build"></span>[A daemon from another build](~clackatoo/reference/compatibility#a-daemon-from-another-build).
