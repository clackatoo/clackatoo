// The app's service worker: it shows the daemon's notifications, and opens
// the chat or terminal one is about when it's tapped. It keeps nothing: the daemon
// serves the app afresh each time, so an upgraded daemon's app is never
// stale.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

// A notification: the chat or terminal it's about, and a line to show. Nothing else is
// ever sent, and it's sealed for this device alone.
self.addEventListener("push", (event) => {
  let said = {};
  try {
    said = event.data ? event.data.json() : {};
  } catch (_) {
    said = {};
  }
  const title = typeof said.title === "string" && said.title ? said.title : "clackatoo";
  const body = typeof said.body === "string" ? said.body : "";
  const chat = Number.isInteger(said.chat) ? said.chat : null;
  const terminal = Number.isInteger(said.terminal) ? said.terminal : null;
  const tag = chat !== null ? `chat-${chat}` : terminal !== null ? `term-${terminal}` : "clackatoo";
  event.waitUntil(
    self.registration.showNotification(title, {
      body,
      tag,
      data: { chat, terminal },
      icon: "/icon-192.png",
      badge: "/icon-192.png",
    }),
  );
});

// Tapped: the app opens on the chat or terminal, in the window it has, or
// a new one.
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const data = event.notification.data || {};
  const chat = Number.isInteger(data.chat) ? data.chat : null;
  const terminal = Number.isInteger(data.terminal) ? data.terminal : null;
  const url = chat !== null ? `/#chat/${chat}` : terminal !== null ? `/#term/${terminal}` : "/";
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      for (const client of windows) {
        if ("focus" in client) {
          if (chat !== null) {
            client.postMessage({ chat });
          } else if (terminal !== null) {
            client.postMessage({ terminal });
          }
          return client.focus();
        }
      }
      return self.clients.openWindow(url);
    }),
  );
});
