// The app's service worker: it shows the daemon's notifications, and opens
// the chat one is about when it's tapped. It keeps nothing: the daemon
// serves the app afresh each time, so an upgraded daemon's app is never
// stale.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

// A notification: the chat it's about, and a line to show. Nothing else is
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
  event.waitUntil(
    self.registration.showNotification(title, {
      body,
      tag: chat === null ? "clackatoo" : `chat-${chat}`,
      data: { chat },
      icon: "/icon-192.png",
      badge: "/icon-192.png",
    }),
  );
});

// Tapped: the app opens on the chat, in the window it has, or a new one.
self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const chat = event.notification.data && event.notification.data.chat;
  const url = chat === null || chat === undefined ? "/" : `/#chat/${chat}`;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      for (const client of windows) {
        if ("focus" in client) {
          if (chat !== null && chat !== undefined) {
            client.postMessage({ chat });
          }
          return client.focus();
        }
      }
      return self.clients.openWindow(url);
    }),
  );
});
