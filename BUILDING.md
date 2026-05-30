# Building Mighty IDE

## Prerequisites

| Tool | What it does | Verified path (Windows) |
|------|--------------|--------------------------|
| **`mty`** (Mighty compiler) | builds `src/main.mty` → `target/main.exe` | `C:\Users\ihass\stardust\target\debug\mty.exe` (v0.36) |
| **Rust** toolchain | builds the `mighty-ui-sys` shim cdylib + the arena runtime | `cargo` on PATH |
| **clang** | the linker `mty build` drives (GNU `-o` arg syntax) | `C:\Program Files\LLVM\bin\clang.exe` |
| **llvm-ar** | archives the arena runtime staticlib | `C:\Program Files\LLVM\bin\llvm-ar.exe` |

`mty build` cannot use MSVC `link.exe` (wrong arg syntax) — **clang is required**.

If `mty` is missing, build it from the [Mighty](https://github.com/hassard0/Mighty) repo:

```sh
cargo build -p mty-cli --bin mty
```

## One-shot build

```sh
./build-ide.sh
```

This:
1. `cargo build -p mighty-ui-sys -p mty-rt-abi` — the Vello/wgpu shim (cdylib) + the bumpalo arena runtime (staticlib)
2. stages `target/debug/mty_rt_abi.lib` → `vendor/mty_rt_abi.lib`
3. copies the shim import lib + DLL next to the output exe
4. `mty build src/main.mty --out-dir target` → `target/main.exe`

## Run

```sh
./target/main.exe path/to/file    # defaults to ./scratch.mty if omitted
```

## Environment variables

- `MTY_LINKER` / `STARDUST_LINKER` — point `mty build` at clang. `build-ide.sh` sets both to the clang path above.
- `ANTHROPIC_API_KEY` — enables the AI copilot panel (Ctrl+Shift+A). Optional.

## Disk / link notes

- Build with **`CARGO_INCREMENTAL=0`** to avoid the large incremental cache:

  ```sh
  CARGO_INCREMENTAL=0 ./build-ide.sh
  ```

- If the link step fails on disk space, clear the incremental cache and retry:

  ```sh
  rm -rf target/debug/incremental
  ```

## Verifying the shim

```sh
CARGO_INCREMENTAL=0 cargo clippy -p mighty-ui-sys     # lint
CARGO_INCREMENTAL=0 cargo test  -p mighty-ui-sys      # ~293 unit/integration tests
```
