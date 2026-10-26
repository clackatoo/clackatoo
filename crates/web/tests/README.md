# Browser checks

`browser.mjs` runs the real Rust/WASM client against the synthetic protocol peer
in `examples/browser_fixture.rs`. It never connects to a daemon, signs in to a
provider, or sends a model prompt. The test server uses the daemon's content
security policy. Screenshots go to `target/web-browser-checks` by default.

From the repository root:

```sh
cargo build -p clackatoo-web --example browser_fixture --locked
cargo build -p clackatoo-web --locked --target wasm32-unknown-unknown --profile web-dev --target-dir target/web
wasm-bindgen --target web --no-typescript --out-name app --out-dir target/browser-preview target/web/wasm32-unknown-unknown/web-dev/clackatoo_web.wasm
WEB_ASSETS=target/browser-preview node crates/web/tests/browser.mjs
```

Install Playwright and its Chromium browser in the test environment first. When
Playwright isn't on Node's module path, set `PLAYWRIGHT_MODULE` to the absolute
path of its `index.mjs`. `CHROMIUM_EXECUTABLE` can select an existing Chromium.
`WEB_SHOTS` changes the screenshot directory.

The checks cover dashboard filtering and search, per-chat drafts across navigation
and reload, rejected and unconfirmed delivery, native steering, queued controls,
global model search, saved favorites, permissions and usage panels, approvals,
desktop sidebar navigation, scroll retention and Latest, background tabs (the
heading's count, the list, a second tap to stop one, bringing one back), and 320px,
390px, landscape, reduced keyboard-sized and desktop viewports. Pure Rust tests cover state
transitions and the absence of automatic message replay after connection loss.

Viewport emulation doesn't reproduce an iPhone's software keyboard or Safari's
browser chrome. Also check on a phone: open/close the keyboard, rotate while
writing, switch away and return, and confirm the composer and sheet close button
stay reachable above the safe area. Test notifications in an installed home-screen
app; desktop Chromium doesn't establish that behavior.
