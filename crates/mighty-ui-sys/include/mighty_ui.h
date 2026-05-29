/* mighty_ui.h — flat C ABI for the Mighty IDE native render shim.
 *
 * Hand-written to mirror crates/mighty-ui-sys/src/{ffi,lib}.rs. The Mighty side
 * generates `extern c` bindings from this surface. The shim owns the window,
 * GPU surface, and text; the IDE owns the main loop and drives the shim each
 * frame (poll/pump model). The shim NEVER calls back into the caller.
 */
#ifndef MIGHTY_UI_H
#define MIGHTY_UI_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- colors -------------------------------------------------------------- */

/* RGBA, each component in 0.0..=1.0. */
typedef struct MuiColor {
    float r;
    float g;
    float b;
    float a;
} MuiColor;

/* ---- event tags (MuiEvent.tag) ------------------------------------------- */

#define MUI_EVENT_NONE        0u
#define MUI_EVENT_CHAR        1u
#define MUI_EVENT_KEY         2u
#define MUI_EVENT_MOUSE_DOWN  3u
#define MUI_EVENT_MOUSE_UP    4u
#define MUI_EVENT_SCROLL      5u
#define MUI_EVENT_RESIZE      6u
#define MUI_EVENT_CLOSE       7u

/* ---- named key codes (MuiEvent.key, when tag == MUI_EVENT_KEY) ----------- */

#define MUI_KEY_UNKNOWN    0u
#define MUI_KEY_LEFT       1u
#define MUI_KEY_RIGHT      2u
#define MUI_KEY_UP         3u
#define MUI_KEY_DOWN       4u
#define MUI_KEY_BACKSPACE  5u
#define MUI_KEY_ENTER      6u
#define MUI_KEY_TAB        7u
#define MUI_KEY_ESCAPE     8u
#define MUI_KEY_DELETE     9u
#define MUI_KEY_HOME       10u
#define MUI_KEY_END        11u
#define MUI_KEY_PAGE_UP    12u
#define MUI_KEY_PAGE_DOWN  13u

/* ---- mouse button codes (MuiEvent.button) -------------------------------- */

#define MUI_MOUSE_LEFT    0u
#define MUI_MOUSE_RIGHT   1u
#define MUI_MOUSE_MIDDLE  2u
#define MUI_MOUSE_OTHER   3u

/* ---- modifier bitflags (MuiEvent.mods) ----------------------------------- */

#define MUI_MOD_SHIFT  (1u << 0)
#define MUI_MOD_CTRL   (1u << 1)
#define MUI_MOD_ALT    (1u << 2)
#define MUI_MOD_SUPER  (1u << 3)

/* A flattened input event. Which fields are meaningful depends on `tag`:
 *   CHAR        -> codepoint, mods
 *   KEY         -> key, mods
 *   MOUSE_DOWN  -> button, x, y, mods
 *   MOUSE_UP    -> button, x, y, mods
 *   SCROLL      -> scroll_x, scroll_y, mods
 *   RESIZE      -> width, height
 *   CLOSE/NONE  -> (none)
 */
typedef struct MuiEvent {
    uint32_t tag;
    uint32_t codepoint; /* Unicode scalar (CHAR) */
    uint32_t key;       /* MUI_KEY_* (KEY) */
    uint32_t button;    /* MUI_MOUSE_* (mouse) */
    uint32_t mods;      /* MUI_MOD_* bitflags */
    float    x;         /* cursor x px (mouse) */
    float    y;         /* cursor y px (mouse) */
    float    scroll_x;  /* wheel dx (scroll) */
    float    scroll_y;  /* wheel dy (scroll) */
    uint32_t width;     /* new width px (resize) */
    uint32_t height;    /* new height px (resize) */
} MuiEvent;

/* Opaque context handle. */
typedef struct MuiContext MuiContext;

/* ---- spike smoke export -------------------------------------------------- */

int32_t mui_smoke_add(int32_t a, int32_t b);

/* ---- init / shutdown (2.1) ----------------------------------------------- */

/* Open a window and set up GPU + text. `title_ptr`/`title_len` is UTF-8.
 * Returns an opaque context, or NULL on failure. */
MuiContext *mui_init(uint32_t width, uint32_t height,
                     const uint8_t *title_ptr, size_t title_len);

/* Free the context and tear down the shim. */
void mui_shutdown(MuiContext *ctx);

/* ---- draw (2.2 / 2.3) ---------------------------------------------------- */

/* Queue a solid-color rect (pixel space) for the current frame. */
void mui_fill_rect(MuiContext *ctx, float x, float y, float w, float h,
                   MuiColor color);

/* Queue UTF-8 text at (x, y) in `color` for the current frame. */
void mui_draw_text(MuiContext *ctx, float x, float y,
                   const uint8_t *utf8_ptr, size_t len, MuiColor color);

/* Measure UTF-8 text; writes pixel extents to *out_w / *out_h. */
bool mui_text_measure(MuiContext *ctx, const uint8_t *utf8_ptr, size_t len,
                      float *out_w, float *out_h);

/* ---- frame lifecycle + clip (2.4) ---------------------------------------- */

/* Begin a frame: acquire the surface texture and reset draw state. */
void mui_begin_frame(MuiContext *ctx);

/* Set a scissor clip rect (pixels) for subsequent draws this frame. */
void mui_set_clip(MuiContext *ctx, uint32_t x, uint32_t y,
                  uint32_t w, uint32_t h);

/* End the frame: submit rects then text, and present. */
void mui_end_frame(MuiContext *ctx);

/* ---- event pump (2.5) ---------------------------------------------------- */

/* Pump OS events, then pop one queued event into *out_ev.
 * Returns true if an event was written, false when the queue is empty. */
bool mui_poll_event(MuiContext *ctx, MuiEvent *out_ev);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* MIGHTY_UI_H */
