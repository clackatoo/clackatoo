// Loads the app: its WebAssembly, and the JavaScript wasm-bindgen made to
// call it. Everything else is the app's own Rust.
import init from "./app.js";

init();
