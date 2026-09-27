//! The `.vyx` template cursor classifier and the vocabularies for tag,
//! attribute, directive, event and component-prop completion.
//!
//! A small scan of raw `.vyx` text, not a re-parse of the generator's grammar.
//! It covers the structural positions, which lower to derived code with no
//! verbatim origin. Interpolations and `Tw` class values go through the
//! origin-map forward mapping instead.

/// What the cursor is positioned to complete in a `.vyx` template.
#[derive(Debug, Clone, PartialEq)]
pub enum VyxCursor {
    /// A tag name right after `<`: sibling components (PascalCase) and,
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
    /// Inside a static `class="..."` value: the whitespace-delimited token under
    /// the cursor and its 1-based start column.
    ClassValue { token: String, start_col: usize },
    /// Script, text, `{{ }}`, or a non-class attribute value.
    Other,
}

/// Classify the cursor at 1-based `(line, col)` in `.vyx` `text`.
pub fn classify(text: &str, line: usize, col: usize) -> VyxCursor {
    let chars: Vec<char> = text.chars().collect();
    let Some(offset) = offset_of(&chars, line, col) else {
        return VyxCursor::Other;
    };

    if in_script(text, offset) {
        return VyxCursor::Other;
    }
    if in_mustache(&chars, offset) {
        return VyxCursor::Other;
    }

    match enclosing_open_tag(&chars, offset) {
        Some(lt) => classify_in_tag(&chars, lt, offset, line),
        None => VyxCursor::Other,
    }
}

/// The index of the `<` beginning the open (non-closing, non-comment) tag that
/// encloses `offset`, or `None` if the cursor is in template text. A `>` inside
/// a quoted value does not close the tag. HTML comments are skipped whole, so an
/// apostrophe in one (`don't`) opens no quote.
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
                    if chars.get(i + 1) == Some(&'!')
                        && chars.get(i + 2) == Some(&'-')
                        && chars.get(i + 3) == Some(&'-')
                    {
                        // Jump past `-->`, or to the cursor if the comment is open.
                        let mut j = i + 4;
                        while j + 2 < offset
                            && !(chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>')
                        {
                            j += 1;
                        }
                        i = if j + 2 < offset { j + 3 } else { offset };
                        continue;
                    }
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
    let mut i = lt + 1;
    let name_start = i;
    while i < offset && !chars[i].is_whitespace() && chars[i] != '/' && chars[i] != '>' {
        i += 1;
    }
    let tag: String = chars[name_start..i].iter().collect();
    let is_component = tag.chars().next().is_some_and(|c| c.is_ascii_uppercase());

    if i >= offset {
        return VyxCursor::TagName {
            prefix: tag,
            start_col: col_of(chars, name_start),
        };
    }

    let mut in_quote: Option<char> = None;
    // The attribute name that the open quoted value belongs to.
    let mut quoted_attr: Option<String> = None;
    let mut word_start = i;
    // The last word before `=`, which names the next value.
    let mut last_word: Option<(usize, usize)> = None; // (start, end)
                                                      // Just past a closed value (`class="x"|`) no attribute name completes:
                                                      // its textEdit would delete the value and its closing quote.
    let mut closed_value = false;
    let mut j = i;
    while j < offset {
        let c = chars[j];
        match in_quote {
            Some(q) => {
                if c == q {
                    in_quote = None;
                    quoted_attr = None;
                    closed_value = true;
                }
            }
            None => {
                closed_value = false;
                if c == '"' || c == '\'' {
                    in_quote = Some(c);
                    quoted_attr = last_word.map(|(s, e)| chars[s..e].iter().collect::<String>());
                    word_start = j + 1;
                } else if c.is_whitespace() || c == '=' || c == '/' {
                    if word_start < j {
                        last_word = Some((word_start, j));
                    }
                    if c.is_whitespace() || c == '/' {
                        word_start = j + 1;
                    }
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
        let attr = quoted_attr.unwrap_or_default();
        if attr == "class" {
            let (token, start) = class_token(chars, word_start, offset);
            return VyxCursor::ClassValue {
                token,
                start_col: col_of(chars, start),
            };
        }
        // Value expressions (`:attr`, `@event`, `v-if`) go through the forward map.
        let _ = line;
        return VyxCursor::Other;
    }

    // `word_start` still points after the opening quote here.
    if closed_value {
        let _ = line;
        return VyxCursor::Other;
    }

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
    while hi < chars.len() && !is_class_boundary(chars[hi]) && chars[hi] != '"' && chars[hi] != '\''
    {
        hi += 1;
    }
    (chars[lo..hi].iter().collect(), lo)
}

fn is_class_boundary(c: char) -> bool {
    c.is_whitespace() || c == '"' || c == '\''
}

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
    // The cursor after the last char of the buffer.
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

/// Whether the char `offset` is inside the `<script> ... </script>` body, as
/// `vyrn_frontend::vyx` bounds it: a `</script>` in a string closes nothing.
fn in_script(text: &str, offset: usize) -> bool {
    let byte_off: usize = text.chars().take(offset).map(|c| c.len_utf8()).sum();
    match vyrn_frontend::vyx::script_body(text) {
        Some((start, end)) => byte_off >= start && byte_off <= end,
        // A `<script>` the file never closes owns everything after it.
        None => text.find("<script>").is_some_and(|o| byte_off > o),
    }
}

/// Whether `offset` is inside a `{{ ... }}` interpolation.
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
    "id",
    "class",
    "style",
    "title",
    "hidden",
    "tabindex",
    "role",
    "lang",
    "dir",
    "draggable",
    "contenteditable",
    "spellcheck",
    "accesskey",
    "aria-label",
    "aria-hidden",
    "data-",
];

/// The attributes one element tag adds to [`GLOBAL_ATTRS`].
pub fn element_attrs(tag: &str) -> &'static [&'static str] {
    match tag {
        "a" => &["href", "target", "rel", "download"],
        "input" => &[
            "type",
            "value",
            "name",
            "placeholder",
            "checked",
            "disabled",
            "readonly",
            "required",
            "min",
            "max",
            "step",
            "pattern",
            "autocomplete",
        ],
        "textarea" => &[
            "value",
            "placeholder",
            "rows",
            "cols",
            "disabled",
            "readonly",
            "required",
        ],
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
    "click",
    "dblclick",
    "input",
    "change",
    "submit",
    "reset",
    "focus",
    "blur",
    "keydown",
    "keyup",
    "keypress",
    "mousedown",
    "mouseup",
    "mousemove",
    "mouseover",
    "mouseout",
    "mouseenter",
    "mouseleave",
    "contextmenu",
    "wheel",
    "scroll",
    "drag",
    "dragstart",
    "dragend",
    "dragover",
    "drop",
    "touchstart",
    "touchmove",
    "touchend",
    "pointerdown",
    "pointerup",
];

/// PascalCase sibling component names in `dir`: basenames of `*.vyx` other than
/// `self_name`, sorted.
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

pub struct Prop {
    pub name: String,
    pub ty: String,
}

/// The props declared in a `.vyx` component's `props { name: Type, ... }` block,
/// by a tolerant scan; empty if the file or block is missing.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `<div class="x"|`: a completion here would delete the value.
    #[test]
    fn a_cursor_right_after_a_closed_value_is_no_attribute_name_slot() {
        let vyx = "<div class=\"x\"";
        assert_eq!(classify(vyx, 1, 15), VyxCursor::Other);
        let closed = "<div class=\"x\"/>";
        assert_eq!(classify(closed, 1, 15), VyxCursor::Other);
        assert!(matches!(classify(vyx, 1, 13), VyxCursor::ClassValue { .. }));
        let spaced = "<div class=\"x\" ";
        assert!(matches!(
            classify(spaced, 1, 16),
            VyxCursor::AttrName { .. }
        ));
    }

    /// `<!-- don't -->` before the element opens no quote.
    #[test]
    fn an_apostrophe_inside_an_html_comment_does_not_poison_the_tag_scan() {
        let vyx = "<!-- don't panic -->\n<div class=\"x\"";
        assert!(matches!(classify(vyx, 2, 13), VyxCursor::ClassValue { .. }));
        let named = "<!-- it's fine -->\n<div ";
        assert!(matches!(classify(named, 2, 6), VyxCursor::AttrName { .. }));
    }
}
