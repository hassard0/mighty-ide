//! Scalar `mui_snippet_*` ABI for prefix → template expansion with navigable
//! tab-stops.
//!
//! The engine + all logic live in [`crate::snippets`]; this is the thin scalar
//! veneer the Mighty loop drives (mirrors `ghostabi` / the completion ABI):
//!
//!   * [`mui_snippet_try_expand`] — at the cursor, if the word before it is a
//!     snippet prefix, expand the body, begin the tab-stop session, and select
//!     the first stop. Returns `1` if it expanded, else `0` (indent instead).
//!   * [`mui_snippet_active`] — `1` while a tab-stop session is in progress.
//!   * [`mui_snippet_next_stop`] / [`mui_snippet_prev_stop`] — Tab / Shift+Tab
//!     navigate the stops; the final Tab jumps to `$0` and ends the session.
//!   * [`mui_snippet_cancel`] — Esc / cursor-left-region ends the session.
//!   * [`mui_snippet_replace_stop`] — when typing begins on a selected
//!     placeholder, delete the placeholder first so the typed text replaces it.
//!   * [`mui_snippet_inject_completions`] — completion-source hook: after a
//!     completion request, prepend the matching snippet prefixes with a distinct
//!     "snippet" badge.
//!   * [`mui_snippet_complete_is`] / [`mui_snippet_complete_expand`] — accepting
//!     a snippet entry from the dropdown expands it (rather than inserting text).

use crate::completion::prefix_at;
use crate::snippets;
use crate::MuiContext;

/// Cast an opaque `i64` handle back to a context reference.
#[inline]
unsafe fn ctx<'a>(handle: i64) -> Option<&'a mut MuiContext> {
    if handle == 0 {
        return None;
    }
    (handle as usize as *mut MuiContext).as_mut()
}

/// Try to expand the snippet whose prefix is the word before the cursor.
///
/// Tab-priority step (4): only call this when no tab-stop session is active, no
/// ghost is shown, and the completion dropdown is closed. Returns `1` if a
/// snippet expanded (the caller redraws + marks dirty); `0` otherwise (the
/// caller should fall back to inserting an indent).
#[no_mangle]
pub extern "C" fn mui_snippet_try_expand(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    let lang = c.language;
    // Split the borrow: the session + the active model are distinct fields.
    let session = &mut c.snippet_session;
    let model = c.tabs.active_model_mut();
    i32::from(snippets::try_expand(model, session, lang))
}

/// `1` while a tab-stop navigation session is active.
#[no_mangle]
pub extern "C" fn mui_snippet_active(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.snippet_session.is_active()))
}

/// Advance to the NEXT tab-stop (Tab while a session is active), selecting its
/// placeholder. The final stop (`$0`) ends the session — its position becomes the
/// cursor. Returns `1` while the session continues, `0` once it ended.
#[no_mangle]
pub extern "C" fn mui_snippet_next_stop(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    match c.snippet_session.next_stop() {
        Some(stop) => {
            c.tabs.active_model_mut().set_selection(stop.start, stop.end);
            1
        }
        None => {
            // Past the final stop: the session is now over. The cursor was already
            // placed at $0 (the last current() before this call selected it); just
            // collapse the selection at the current cursor.
            let m = c.tabs.active_model_mut();
            let (l, col) = (m.cursor_line(), m.cursor_col());
            m.set_selection((l, col), (l, col));
            0
        }
    }
}

/// Step back to the PREVIOUS tab-stop (Shift+Tab), selecting its placeholder.
/// Clamps at the first stop. Returns `1` while the session is active.
#[no_mangle]
pub extern "C" fn mui_snippet_prev_stop(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if let Some(stop) = c.snippet_session.prev_stop() {
        c.tabs.active_model_mut().set_selection(stop.start, stop.end);
        i32::from(c.snippet_session.is_active())
    } else {
        0
    }
}

/// End the tab-stop session (Esc / the cursor left the snippet region). Leaves
/// the model untouched (the text stays; only navigation stops).
#[no_mangle]
pub extern "C" fn mui_snippet_cancel(handle: i64) {
    if let Some(c) = unsafe { ctx(handle) } {
        c.snippet_session.cancel();
    }
}

/// If a session is active and the current stop's placeholder is selected, delete
/// that selection so the about-to-be-typed text replaces it. Call right before
/// inserting a typed char while in snippet mode. Returns `1` if it removed a
/// selection. (After this, the normal insert path runs as usual.)
#[no_mangle]
pub extern "C" fn mui_snippet_replace_stop(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    if !c.snippet_session.is_active() {
        return 0;
    }
    i32::from(c.tabs.active_model_mut().delete_selection())
}

/// Completion-source hook: after a completion request, prepend the snippet
/// prefixes that match the current completion prefix (with a distinct badge).
/// Reuses the same prefix the completion engine computed from `complete_buf`.
/// No-op when the dropdown isn't populated. Call right after
/// `mui_ed_complete_request`.
#[no_mangle]
pub extern "C" fn mui_snippet_inject_completions(handle: i64) {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return;
    };
    // The prefix the completion engine matched on (run of ident bytes before the
    // cursor in the streamed buffer).
    let cursor = c.complete_buf.len();
    let prefix = prefix_at(&c.complete_buf, cursor);
    if prefix.is_empty() {
        return;
    }
    // Any snippet whose prefix starts with (or equals) the typed prefix — an
    // exact match is still useful (accepting it expands the body).
    let matches: Vec<String> = snippets::snippets_for(c.language)
        .into_iter()
        .filter(|d| d.prefix.starts_with(&prefix))
        .map(|d| d.prefix)
        .collect();
    if matches.is_empty() {
        return;
    }
    c.complete.inject_snippets(&matches);
}

/// `1` if the currently-selected completion candidate is a snippet (so the
/// accept path should EXPAND it rather than insert the literal label).
#[no_mangle]
pub extern "C" fn mui_snippet_complete_is(handle: i64) -> i32 {
    unsafe { ctx(handle) }.map_or(0, |c| i32::from(c.complete.accepted_is_snippet()))
}

/// Expand the accepted snippet completion: delete the typed prefix, insert the
/// snippet body (the selected candidate's text is the prefix), and begin the
/// tab-stop session. Returns `1` on success. Use instead of
/// `mui_ed_complete_accept` when [`mui_snippet_complete_is`] is `1`.
#[no_mangle]
pub extern "C" fn mui_snippet_complete_expand(handle: i64) -> i32 {
    let Some(c) = (unsafe { ctx(handle) }) else {
        return 0;
    };
    // The selected candidate's text is the snippet prefix. Delete what the user
    // typed so far (the completion prefix length), then re-type the full prefix
    // so the cursor is positioned exactly after it, and expand.
    let typed = c.complete.prefix_len();
    let full = c.complete.accepted_text().to_string();
    if full.is_empty() {
        return 0;
    }
    let lang = c.language;
    {
        let m = c.tabs.active_model_mut();
        for _ in 0..typed {
            m.backspace();
        }
        for ch in full.chars() {
            m.insert_char(ch);
        }
    }
    let session = &mut c.snippet_session;
    let model = c.tabs.active_model_mut();
    i32::from(snippets::try_expand(model, session, lang))
}

#[cfg(test)]
mod tests {
    // The engine logic is exhaustively tested in `crate::snippets`. These ABI fns
    // are thin veneers; a null-handle smoke test confirms they never deref null.
    use super::*;

    #[test]
    fn null_handle_is_safe() {
        assert_eq!(mui_snippet_try_expand(0), 0);
        assert_eq!(mui_snippet_active(0), 0);
        assert_eq!(mui_snippet_next_stop(0), 0);
        assert_eq!(mui_snippet_prev_stop(0), 0);
        mui_snippet_cancel(0);
        assert_eq!(mui_snippet_replace_stop(0), 0);
        mui_snippet_inject_completions(0);
        assert_eq!(mui_snippet_complete_is(0), 0);
        assert_eq!(mui_snippet_complete_expand(0), 0);
    }
}
