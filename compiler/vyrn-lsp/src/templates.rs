//! RFC-0042 — `.vyx` template cursor classifier and the discovery vocabularies
//! for attribute / directive / event / component-tag / component-prop completion.
//!
//! This is the editor-only structural knowledge of the `.vyx` template surface:
//! given a cursor offset in raw `.vyx` text, decide what is being typed
//! (`AttrName` / `EventName` / `TagName` / `ClassValue` / `Other`) so the server
//! can offer the right vocabulary. It mirrors the locked v2 template shape
//! (`<script>`/`<template>` sections, `{{ … }}` interpolation, element and
//! PascalCase component tags, `class="…"` / `:attr="…"` / `@event="…"` /  `v-*`
//! attributes) — a small scan, never a re-parse of the generator's grammar.
//!
//! `Tw` class values and `{{ expr }}` interpolations are answered through the
//! RFC-0033 origin-map forward mapping (verbatim regions), not here; this module
//! covers the *structural* positions (attribute names, event names, tags, props)
//! that lower to derived code with no verbatim origin to map into.

/// What the cursor is positioned to complete in a `.vyx` template.
#[derive(Debug, Clone, PartialEq)]
pub enum VyxCursor {
    /// A tag name right after `<` — offer sibling components (PascalCase) and,
    /// for a lowercase prefix, HTML element names.
    TagName { prefix: String, start_col: usize },
    /// An attribute-name position on an element/component tag (not `@`, not a
    /// value). `is_component` selects props vs HTML attributes.
    AttrName {
        tag: String,
        prefix: String,
        is_component: bool,
        start_col: usize,
    },
    /// An `@event` attribute name (the `@` prefix stripped).
    EventName { prefix: String, start_col: usize },
    /// Inside a static `class="…"` value: the whitespace-delimited token under the
    /// cursor and where it starts (1-based col), for token-in-sequence replace.
    ClassValue { token: String, start_col: usize },
    /// Anywhere the structural surfaces do not apply (script, text, `{{ }}`,
    /// a non-class attribute value) — handled by the forward-map / other paths.
    Other,
}

/// Classify the cursor at 1-based `(line, col)` in `.vyx` `text`.
pub fn classify(text: &str, line: usize, col: usize) -> VyxCursor {
    let chars: Vec<char> = text.chars().collect();
    let Some(offset) = offset_of(&chars, line, col) else {
        return VyxCursor::Other;
    };

    // Inside `<script> … </script>`? Structural template completion does not apply.
    if in_script(text, offset) {
        return VyxCursor::Other;
    }
    // Inside a `{{ … }}` interpolation? Expression completion (forward map) owns it.
    if in_mustache(&chars, offset) {
        return VyxCursor::Other;
    }

    // Find the open tag enclosing the cursor, if any, by scanning forward and
    // tracking tag/quote state — so a `>` inside an attribute value string
    // (`v-if="a > b"`) is not mistaken for the tag close.
    match enclosing_open_tag(&chars, offset) {
        Some(lt) => classify_in_tag(&chars, lt, offset, line),
        None => VyxCursor::Other,
    }
}

/// The index of the `<` beginning the open (non-closing, non-comment) tag that
/// encloses `offset`, or `None` if the cursor is in template text. Quote state is
/// tracked so a `>` inside an attribute value does not close the tag.
fn enclosing_open_tag(chars: &[char], offset: usize) -> Option<usize> {
    let mut in_tag: Option<usize> = None; // Some(start) while inside `<...>`
    let mut is_open = false; // the current tag is an element open tag
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < offset {
        let c = chars[i];
        match in_tag {
            None => {
                if c == '<' {
                    let nxt = chars.get(i + 1).copied();
                    is_open = nxt != Some('/') && nxt != Some('!');
                    in_tag = Some(i);
                }
            }
            Some(_) => match quote {
                Some(q) => {
                    if c == q {
                        quote = None;
                    }
                }
                None => {
                    if c == '"' || c == '\'' {
                        quote = Some(c);
                    } else if c == '>' {
                        in_tag = None;
                    }
                }
            },
        }
        i += 1;
    }
    match in_tag {
        Some(start) if is_open => Some(start),
        _ => None,
    }
}

/// Classify a cursor known to be inside the open tag beginning at `lt` (`<`).
fn classify_in_tag(chars: &[char], lt: usize, offset: usize, line: usize) -> VyxCursor {
    // Tag name: from just after `<` to the first whitespace / `/` / `>`.
    let mut i = lt + 1;
    let name_start = i;
    while i < offset && !chars[i].is_whitespace() && chars[i] != '/' && chars[i] != '>' {
        i += 1;
    }
    let tag: String = chars[name_start..i].iter().collect();
    let is_component = tag.chars().next().is_some_and(|c| c.is_ascii_uppercase());

    // Still within the tag name (no whitespace seen yet) → naming the tag.
    if i >= offset {
        return VyxCursor::TagName {
            prefix: tag,
            start_col: col_of(chars, name_start),
        };
    }

    // Scan the attribute region, tracking quote state and the current word.
    let mut in_quote: Option<char> = None;
    // The attribute name that the currently-open quoted value belongs to.
    let mut quoted_attr: Option<String> = None;
    // Start index (in `chars`) of the current unquoted word (attribute name).
    let mut word_start = i;
    // The last attribute name token seen (word before `=`), to name a value.
    let mut last_word: Option<(usize, usize)> = None; // (start, end)
    let mut j = i;
    while j < offset {
        let c = chars[j];
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                    quoted_attr = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    in_quote = Some(c);
                    quoted_attr = last_word
                        .map(|(s, e)| chars[s..e].iter().collect::<String>());
                    word_start = j + 1;
                } else if c.is_whitespace() || c == '=' || c == '/' {
                    if word_start < j {
                        last_word = Some((word_start, j));
                    }
                    if c.is_whitespace() || c == '/' {
                        word_start = j + 1;
                    }
                    // On `=`, keep `word_start` so the value quote picks up the name.
                    if c == '=' {
                        word_start = j + 1;
                    }
                } else if word_start > j {
                    word_start = j;
                }
            }
        }
        j += 1;
    }

    if let Some(_q) = in_quote {
        // Inside an attribute value string.
        let attr = quoted_attr.unwrap_or_default();
        if attr == "class" {
            let (token, start) = class_token(chars, word_start, offset);
            return VyxCursor::ClassValue {
                token,
                start_col: col_of(chars, start),
            };
        }
        // `:class`, `@event`, `:attr`, `v-if` value expressions are handled by the
        // forward-map expression path, not structural completion.
        let _ = line;
        return VyxCursor::Other;
    }

    // An attribute-name position: the current partial word from `word_start`.
    let prefix: String = chars[word_start..offset].iter().collect();
    if let Some(rest) = prefix.strip_prefix('@') {
        return VyxCursor::EventName {
            prefix: rest.to_string(),
            start_col: col_of(chars, word_start),
        };
    }
    VyxCursor::AttrName {
        tag,
        prefix,
        is_component,
        start_col: col_of(chars, word_start),
    }
}

/// The whitespace-delimited class token containing `offset`, and its start index.
fn class_token(chars: &[char], value_start: usize, offset: usize) -> (String, usize) {
    let mut lo = offset;
    while lo > value_start && !is_class_boundary(chars[lo - 1]) {
        lo -= 1;
    }
    let mut hi = offset;
    while hi < chars.len() && !is_class_boundary(chars[hi]) && chars[hi] != '"' && chars[hi] != '\'' {
        hi += 1;
    }
    (chars[lo..hi].iter().collect(), lo)
}

fn is_class_boundary(c: char) -> bool {
    c.is_whitespace() || c == '"' || c == '\''
}

// --------------------------------------------------------------------------
// small scan helpers (offsets into the flat char vector)
// --------------------------------------------------------------------------

/// The char offset of 1-based `(line, col)`, or `None` if out of range.
fn offset_of(chars: &[char], line: usize, col: usize) -> Option<usize> {
    let mut cur_line = 1usize;
    let mut cur_col = 1usize;
    for (idx, &c) in chars.iter().enumerate() {
        if cur_line == line && cur_col == col {
            return Some(idx);
        }
        if c == '\n' {
            cur_line += 1;
            cur_col = 1;
        } else {
            cur_col += 1;
        }
    }
    // End-of-buffer position (cursor after the last char on its line).
    if cur_line == line && cur_col == col {
        return Some(chars.len());
    }
    None
}

/// The 1-based column of char index `idx`.
fn col_of(chars: &[char], idx: usize) -> usize {
    let mut col = 1usize;
    for &c in chars.iter().take(idx) {
        if c == '\n' {
            col = 1;
        } else {
            col += 1;
        }
    }
    col
}

/// Whether `offset` is inside a `<script> … </script>` region.
fn in_script(text: &str, offset: usize) -> bool {
    // Compare byte offsets: rebuild a byte offset from the char offset.
    let byte_off: usize = text.chars().take(offset).map(|c| c.len_utf8()).sum();
    let before = &text[..byte_off.min(text.len())];
    let open = before.rfind("<script");
    let close = before.rfind("</script>");
    match (open, close) {
        (Some(o), Some(c)) => o > c,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Whether `offset` is inside a `{{ … }}` interpolation.
fn in_mustache(chars: &[char], offset: usize) -> bool {
    let s: String = chars[..offset.min(chars.len())].iter().collect();
    let open = s.rfind("{{");
    let close = s.rfind("}}");
    match (open, close) {
        (Some(o), Some(c)) => o > c,
        (Some(_), None) => true,
        _ => false,
    }
}

// --------------------------------------------------------------------------
// discovery vocabularies (RFC-0042 B: attributes / directives / events)
// --------------------------------------------------------------------------

/// The Vyrn template directives offered at an attribute-name position.
pub const DIRECTIVES: &[(&str, &str)] = &[
    ("v-if", "conditional render"),
    ("v-else-if", "conditional render (chained)"),
    ("v-else", "conditional render (fallback)"),
    ("v-for", "list render"),
    ("v-html", "raw inner HTML"),
    (":key", "keyed-list identity"),
    (":", "dynamic attribute (: prefix)"),
    ("@", "event handler (@ prefix)"),
];

/// Global HTML attributes offered on any element.
pub const GLOBAL_ATTRS: &[&str] = &[
    "id", "class", "style", "title", "hidden", "tabindex", "role", "lang", "dir",
    "draggable", "contenteditable", "spellcheck", "accesskey", "aria-label",
    "aria-hidden", "data-",
];

/// Per-element attribute refinements (element tag → extra attributes).
pub fn element_attrs(tag: &str) -> &'static [&'static str] {
    match tag {
        "a" => &["href", "target", "rel", "download"],
        "input" => &[
            "type", "value", "name", "placeholder", "checked", "disabled",
            "readonly", "required", "min", "max", "step", "pattern", "autocomplete",
        ],
        "textarea" => &["value", "placeholder", "rows", "cols", "disabled", "readonly", "required"],
        "select" => &["value", "name", "disabled", "required", "multiple"],
        "option" => &["value", "selected", "disabled"],
        "button" => &["type", "disabled", "name", "value"],
        "form" => &["action", "method", "novalidate"],
        "label" => &["for"],
        "img" => &["src", "alt", "width", "height", "loading"],
        "video" | "audio" => &["src", "controls", "autoplay", "loop", "muted", "preload"],
        "source" => &["src", "type", "srcset", "media"],
        "table" => &["summary"],
        "td" | "th" => &["colspan", "rowspan", "headers", "scope"],
        "meta" => &["name", "content", "charset", "property"],
        "link" => &["href", "rel", "type", "media"],
        "script" => &["src", "type", "async", "defer"],
        _ => &[],
    }
}

/// DOM events the runtime dispatches, offered after `@`.
pub const EVENTS: &[&str] = &[
    "click", "dblclick", "input", "change", "submit", "reset", "focus", "blur",
    "keydown", "keyup", "keypress", "mousedown", "mouseup", "mousemove",
    "mouseover", "mouseout", "mouseenter", "mouseleave", "contextmenu", "wheel",
    "scroll", "drag", "dragstart", "dragend", "dragover", "drop", "touchstart",
    "touchmove", "touchend", "pointerdown", "pointerup",
];

// --------------------------------------------------------------------------
// component discovery (RFC-0042 C: sibling PascalCase `.vyx` + their props)
// --------------------------------------------------------------------------

/// PascalCase sibling component names in `dir` (basenames of `*.vyx`, excluding
/// `self_name`). These are the tags `<Cap…` the generator would resolve.
pub fn sibling_components(dir: &std::path::Path, self_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(base) = name.strip_suffix(".vyx") {
            if base == self_name {
                continue;
            }
            if base.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
                out.push(base.to_string());
            }
        }
    }
    out.sort();
    out
}

/// A component prop: name and declared type text.
pub struct Prop {
    pub name: String,
    pub ty: String,
}

/// Parse the `props { … }` block out of a sibling `.vyx` component's `<script>`,
/// returning each declared prop's name and type text. A small tolerant scan (the
/// block is `props { name: Type, name2: Type2 }`), not a full parse.
pub fn component_props(vyx_path: &std::path::Path) -> Vec<Prop> {
    let Ok(text) = std::fs::read_to_string(vyx_path) else {
        return Vec::new();
    };
    let Some(at) = text.find("props") else {
        return Vec::new();
    };
    let rest = &text[at + "props".len()..];
    let Some(open) = rest.find('{') else {
        return Vec::new();
    };
    // Find the matching close brace.
    let mut depth = 0i32;
    let mut end = None;
    for (idx, c) in rest[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + idx);
                    break;
                }
            }
            _ => {}
        }
    }
    let Some(end) = end else {
        return Vec::new();
    };
    let body = &rest[open + 1..end];
    let mut props = Vec::new();
    for field in body.split(',') {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        if let Some((name, ty)) = field.split_once(':') {
            let name = name.trim();
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            props.push(Prop {
                name: name.to_string(),
                ty: ty.trim().to_string(),
            });
        }
    }
    props
}
