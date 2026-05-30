// webspin — JS host shim for the Mighty "Run in Browser" sample.
//
// This is the "outside" half of the v0.22-era browser pattern (the same
// one the `web-game` template + `demos/05_notetris_web` use). It:
//
//   1. fetches `/main.wasm` (served by `mty serve`),
//   2. extracts the embedded core module from the Component Model
//      envelope — browsers don't execute components natively yet,
//   3. instantiates with a `log` import the Mighty agent uses to emit
//      `evt:…` lines,
//   4. calls the wasm-exported `tick()` every animation frame,
//   5. advances + renders a spinning arc + frame counter from the
//      `evt:spin` lines the guest logs.
//
// When the canvas WIT (`mty:web/canvas@0.1`) lands the guest will own
// the draws and this file shrinks to input plumbing. `mty serve --watch`
// also opens a `/_reload` websocket; on `reload` we `location.reload()`.

// ---- Canvas refs ----------------------------------------------------
const canvas = document.getElementById('board');
const ctx = canvas.getContext('2d');
const frameEl = document.getElementById('frames');
const logEl = document.getElementById('log');

// ---- Component-model loader (same trick as demos/05_notetris_web) --
async function loadWasm() {
  const resp = await fetch('./main.wasm');
  if (!resp.ok) throw new Error('failed to fetch /main.wasm: ' + resp.status);
  const buf = new Uint8Array(await resp.arrayBuffer());
  return findCoreModule(buf);
}

function findCoreModule(bytes) {
  // Components start with `\0asm\x0d\x00\x01\x00`; core modules with
  // `\0asm\x01\x00\x00\x00`. Browsers refuse the former, so we scan for
  // the latter and hand `WebAssembly.instantiate` the sub-buffer.
  for (let i = 0; i < bytes.length - 8; i++) {
    if (bytes[i] === 0x00 && bytes[i+1] === 0x61 &&
        bytes[i+2] === 0x73 && bytes[i+3] === 0x6d &&
        bytes[i+4] === 0x01 && bytes[i+5] === 0x00 &&
        bytes[i+6] === 0x00 && bytes[i+7] === 0x00) {
      return bytes.subarray(i);
    }
  }
  throw new Error('no core wasm preamble found inside the component');
}

// ---- Log stream -----------------------------------------------------
function appendLog(line) {
  if (!logEl) return;
  logEl.textContent += line + '\n';
  if (logEl.textContent.length > 4000) {
    logEl.textContent = logEl.textContent.slice(-3000);
  }
  logEl.scrollTop = logEl.scrollHeight;
}

// ---- Spinner state, advanced by the guest's `evt:spin` lines --------
const W = canvas.width;
const H = canvas.height;
const CX = W / 2;
const CY = H / 2;
const R = Math.min(W, H) * 0.32;
let angle = 0;       // radians
let frames = 0;

function draw() {
  // Backdrop.
  ctx.fillStyle = '#0b0d12';
  ctx.fillRect(0, 0, W, H);

  // Faint track ring.
  ctx.beginPath();
  ctx.arc(CX, CY, R, 0, Math.PI * 2);
  ctx.strokeStyle = '#1d2230';
  ctx.lineWidth = 10;
  ctx.stroke();

  // Spinning accent arc (¾ sweep) rotating with `angle`.
  ctx.beginPath();
  ctx.arc(CX, CY, R, angle, angle + Math.PI * 1.5);
  ctx.strokeStyle = '#b66bff';
  ctx.lineWidth = 10;
  ctx.lineCap = 'round';
  ctx.stroke();

  // Leading dot.
  const dx = CX + Math.cos(angle) * R;
  const dy = CY + Math.sin(angle) * R;
  ctx.beginPath();
  ctx.arc(dx, dy, 9, 0, Math.PI * 2);
  ctx.fillStyle = '#e7ecf3';
  ctx.fill();

  // Center label: the frame counter the guest is driving.
  ctx.fillStyle = '#7f8a9c';
  ctx.font = '600 13px ui-monospace, monospace';
  ctx.textAlign = 'center';
  ctx.fillText('webspin', CX, CY - 6);
  ctx.fillStyle = '#e7ecf3';
  ctx.font = '600 22px ui-monospace, monospace';
  ctx.fillText(String(frames), CX, CY + 18);
}

function handleEvent(kind) {
  switch (kind) {
    case 'spin':
      angle = (angle + 0.10) % (Math.PI * 2);
      frames += 1;
      if (frameEl) frameEl.textContent = frames;
      break;
    case 'reset':
      angle = 0;
      frames = 0;
      if (frameEl) frameEl.textContent = 0;
      break;
    default:
      break;
  }
  draw();
}

function logImport(msg) {
  appendLog(msg);
  if (typeof msg === 'string' && msg.startsWith('evt:')) {
    handleEvent(msg.slice(4));
  }
}

// ---- Decode the imported wasm string -------------------------------
// The Mighty component lowers `log(msg)` as a host import taking
// `(ptr: i32, len: i32)` over linear memory; read it back as UTF-8.
function makeLogImport(getMemory) {
  return (ptr, len) => {
    const mem = getMemory();
    if (!mem) return;
    const bytes = new Uint8Array(mem.buffer, ptr, len);
    logImport(new TextDecoder('utf-8').decode(bytes));
  };
}

// ---- Boot ----------------------------------------------------------
async function boot() {
  let instance;
  const memBox = { instance: null };
  const imp = makeLogImport(() => memBox.instance && memBox.instance.exports.memory);
  const importObj = { env: { log: imp }, mty: { log: imp } };
  try {
    const bytes = await loadWasm();
    const { instance: inst } = await WebAssembly.instantiate(bytes, importObj);
    instance = inst;
    memBox.instance = inst;
  } catch (e) {
    appendLog('[host] failed to load wasm: ' + e);
    return;
  }

  if (typeof instance.exports.start === 'function') instance.exports.start();

  // R restarts the spinner.
  window.addEventListener('keydown', (e) => {
    if ((e.key === 'r' || e.key === 'R') && instance.exports.reset) instance.exports.reset();
  });

  // Drive the guest one tick per animation frame.
  function frame() {
    if (instance.exports.tick) instance.exports.tick();
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);

  draw();
}

// ---- `mty serve --watch` reload websocket --------------------------
function connectReload() {
  try {
    const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
    const ws = new WebSocket(`${proto}//${location.host}/_reload`);
    ws.addEventListener('message', (ev) => {
      if (typeof ev.data === 'string' && ev.data.includes('reload')) location.reload();
    });
    ws.addEventListener('close', () => setTimeout(connectReload, 1000));
  } catch (e) { /* silent */ }
}

boot();
connectReload();
