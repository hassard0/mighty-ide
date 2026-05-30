# webspin — Mighty IDE "Run in Browser" sample

A **pure (non-FFI) Mighty program** that compiles to `wasm32-web` (the
Component Model, Mighty's default target) and animates a spinning arc +
a frame counter in the browser. It's the sample the Mighty IDE's
**Run in Browser** action (Alt+W / palette "Mighty: Run in Browser")
launches.

Pure Mighty only — every handler/export `log`s a literal `evt:…` line,
so nothing crosses an FFI boundary and `mty build --target wasm32-web`
produces a browser-runnable component. (Programs that call `extern "C"`
can't wasm-run; keep web samples pure.)

## Run it from the IDE

Open `examples/webspin/src/main.mty`, press **Alt+W** (or run the
palette command **Mighty: Run in Browser**). The IDE detects the
`mighty.toml` package, spawns `mty serve` on a background thread, finds
the served `http://127.0.0.1:<port>` URL in its output, and opens your
default browser there. The Web panel streams the build/serve output and
offers a **Stop server** affordance.

## Run it from the CLI

```bash
mty serve --watch          # build → serve web/ + main.wasm → hot reload
```

Then open <http://localhost:8000>. `--watch` rebuilds on every edit to
`src/` and pushes a reload over a `/_reload` websocket.

Or build the component without serving:

```bash
mty build --target wasm32-web src/main.mty   # → target/main.wasm
```

## Layout

```
webspin/
├── mighty.toml          # package manifest
├── src/
│   └── main.mty         # the Spin agent + start/tick/reset exports
└── web/
    ├── index.html       # host page (canvas + frame counter + log)
    └── dom-shim.js      # JS host: loads the component, drives tick()
```

## How it works

- `src/main.mty` defines a `Spin` agent + exported `start` / `tick` /
  `reset`. Each `tick` `log`s `evt:spin`; `reset` `log`s `evt:reset`.
- `web/dom-shim.js` fetches `/main.wasm`, extracts the core module from
  the Component Model envelope (browsers don't run components natively
  yet), instantiates it with a `log` import, calls `tick()` per
  animation frame, and renders the spinner from the `evt:spin` lines.

The arithmetic lives host-side because the v0.36 `wasm32-web` target
does not yet lower number→string formatting for `log`; keeping the guest
to literal `log`s is what makes it build cleanly. When the
`mty:web/canvas@0.1` WIT binding lands (v0.24) the guest can paint the
canvas directly and the JS mirror collapses to input plumbing.
