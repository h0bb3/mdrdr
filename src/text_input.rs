//! Small helpers for the in-app single-line text inputs (Ctrl+F search
//! box, Ctrl+P quick-open box). The inputs are stored as
//! `(text: String, cursor: usize, anchor: usize)` triples on whichever
//! UI struct owns them; functions here mutate those in place so we don't
//! need a wrapper type or a separate field group.
//!
//! - `cursor` and `anchor` are *char* indices into `text`, not byte
//!   offsets. Editing UTF-8 by byte offsets is a hazard; chars stay
//!   safe for everything we accept (no shaped/composed scripts yet).
//! - When `cursor == anchor` there's no selection. Drawing code
//!   should still draw the caret at `cursor`.
//! - "Word" boundaries are alphanumeric runs separated by anything
//!   else. Good enough for path / URL / English search queries; we'll
//!   refine if a real script-shaped pipeline lands.

/// Convert a char index into a byte index in `text`. Saturates at the
/// end of `text` rather than panicking on out-of-range input — the UI
/// can clamp lazily and rely on this to do the right thing.
fn byte_index(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(text.len())
}

fn char_count(text: &str) -> usize {
    text.chars().count()
}

fn selection_range(cursor: usize, anchor: usize) -> (usize, usize) {
    if cursor < anchor { (cursor, anchor) } else { (anchor, cursor) }
}

pub fn has_selection(cursor: usize, anchor: usize) -> bool {
    cursor != anchor
}

/// If a selection is active, delete the selected range and collapse the
/// cursor to the start of the selection. Returns true if anything was
/// deleted.
pub fn delete_selection(text: &mut String, cursor: &mut usize, anchor: &mut usize) -> bool {
    if *cursor == *anchor {
        return false;
    }
    let (a, b) = selection_range(*cursor, *anchor);
    let ba = byte_index(text, a);
    let bb = byte_index(text, b);
    text.replace_range(ba..bb, "");
    *cursor = a;
    *anchor = a;
    true
}

/// Insert at the cursor. Replaces the selection if any. Cursor lands
/// after the inserted text; anchor follows.
pub fn insert_str(text: &mut String, cursor: &mut usize, anchor: &mut usize, s: &str) {
    delete_selection(text, cursor, anchor);
    let bi = byte_index(text, *cursor);
    text.insert_str(bi, s);
    let added = s.chars().count();
    *cursor += added;
    *anchor = *cursor;
}

/// Backspace: delete selection if any, otherwise the char before the
/// cursor.
pub fn backspace(text: &mut String, cursor: &mut usize, anchor: &mut usize) {
    if delete_selection(text, cursor, anchor) {
        return;
    }
    if *cursor == 0 {
        return;
    }
    let prev = *cursor - 1;
    let bp = byte_index(text, prev);
    let bc = byte_index(text, *cursor);
    text.replace_range(bp..bc, "");
    *cursor = prev;
    *anchor = prev;
}

/// Forward-delete: delete selection if any, otherwise the char after
/// the cursor.
pub fn delete_forward(text: &mut String, cursor: &mut usize, anchor: &mut usize) {
    if delete_selection(text, cursor, anchor) {
        return;
    }
    let n = char_count(text);
    if *cursor >= n {
        return;
    }
    let bc = byte_index(text, *cursor);
    let bn = byte_index(text, *cursor + 1);
    text.replace_range(bc..bn, "");
}

/// Move left by one char. With `select=true`, extends the selection
/// (anchor stays put). Without, collapses any active selection to the
/// left edge.
pub fn move_left(text: &str, cursor: &mut usize, anchor: &mut usize, select: bool) {
    if !select && has_selection(*cursor, *anchor) {
        let (a, _) = selection_range(*cursor, *anchor);
        *cursor = a;
        *anchor = a;
        return;
    }
    if *cursor > 0 {
        *cursor -= 1;
    }
    if !select {
        *anchor = *cursor;
    }
    let _ = text; // text not needed for left-by-1 but keeps signature consistent
}

/// Move right by one char. Mirror of `move_left`.
pub fn move_right(text: &str, cursor: &mut usize, anchor: &mut usize, select: bool) {
    if !select && has_selection(*cursor, *anchor) {
        let (_, b) = selection_range(*cursor, *anchor);
        *cursor = b;
        *anchor = b;
        return;
    }
    let n = char_count(text);
    if *cursor < n {
        *cursor += 1;
    }
    if !select {
        *anchor = *cursor;
    }
}

/// Word-skip left. Skip any non-alphanumeric run, then skip the
/// alphanumeric run before that. Standard editor behaviour.
pub fn move_word_left(text: &str, cursor: &mut usize, anchor: &mut usize, select: bool) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = *cursor;
    while i > 0 && !chars[i - 1].is_alphanumeric() {
        i -= 1;
    }
    while i > 0 && chars[i - 1].is_alphanumeric() {
        i -= 1;
    }
    *cursor = i;
    if !select {
        *anchor = *cursor;
    }
}

/// Word-skip right. Mirror of `move_word_left`.
pub fn move_word_right(text: &str, cursor: &mut usize, anchor: &mut usize, select: bool) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = *cursor;
    while i < n && !chars[i].is_alphanumeric() {
        i += 1;
    }
    while i < n && chars[i].is_alphanumeric() {
        i += 1;
    }
    *cursor = i;
    if !select {
        *anchor = *cursor;
    }
}

pub fn move_home(cursor: &mut usize, anchor: &mut usize, select: bool) {
    *cursor = 0;
    if !select {
        *anchor = 0;
    }
}

pub fn move_end(text: &str, cursor: &mut usize, anchor: &mut usize, select: bool) {
    *cursor = char_count(text);
    if !select {
        *anchor = *cursor;
    }
}

pub fn select_all(text: &str, cursor: &mut usize, anchor: &mut usize) {
    *anchor = 0;
    *cursor = char_count(text);
}

/// Slice of `text` between cursor and anchor as a fresh String. Empty
/// if no selection is active.
pub fn selected_text(text: &str, cursor: usize, anchor: usize) -> String {
    if cursor == anchor {
        return String::new();
    }
    let (a, b) = selection_range(cursor, anchor);
    let ba = byte_index(text, a);
    let bb = byte_index(text, b);
    text[ba..bb].to_string()
}

/// Clamp cursor and anchor into `0..=char_count(text)`. Use after any
/// external mutation of `text` (e.g. live-reload editing) so the
/// editor model stays consistent.
pub fn clamp(text: &str, cursor: &mut usize, anchor: &mut usize) {
    let n = char_count(text);
    if *cursor > n {
        *cursor = n;
    }
    if *anchor > n {
        *anchor = n;
    }
}
