//! The `{{ }}` TEMPLATING scan - **one implementation, for every consumer of
//! [`Node::Text`]**.
//!
//! [`Node::Text`]'s own doc already says the placeholders are part of that
//! node's verbatim content, so the scan that reads them belongs beside the
//! node, not beside one of its readers. It used to live beside two of them:
//! `highbay_data::content::substitute` resolved the FULL trimmed path, and a
//! private copy in `libhbui::app` resolved only `path.split('.').next()`, so
//! `{{user.email}}` looked up `user` on one path and `user.email` on the
//! other. Nothing in the tree spells a dotted placeholder today, which is the
//! only reason that never surfaced as a bug - the two answers were already
//! different, silently, and a single dotted `{{ }}` would have decided which
//! renderer was right. The full path wins, and there is one scanner now.
//!
//! Neither of those crates could host it: `highbay_data` and `libhbui` are
//! SIBLINGS (both depend on this crate, neither on the other), so a shared
//! home in either would have been a new edge between the document layer and
//! the renderer. This crate is the one both already see.
//!
//! Both functions are pure `&str -> …` with no model of what the text is FOR,
//! which is what lets them sit under a run model, a property sheet and an
//! attribute rewrite alike.
//!
//! [`Node::Text`]: crate::dag::Node::Text

/// Replace `{{ path }}` placeholders using `lookup`. Unknown paths resolve to
/// the empty string. Non-placeholder text (including stray braces) passes
/// through verbatim.
///
/// The path is the WHOLE trimmed text between the braces: `{{user.email}}`
/// asks `lookup` for `user.email`, and a lookup that wants to walk the dots
/// does so itself, where it knows what the segments mean.
pub fn substitute(template: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'{'
            && let Some(close) = template[i + 2..].find("}}")
        {
            let path = template[i + 2..i + 2 + close].trim();
            out.push_str(&lookup(path).unwrap_or_default());
            i = i + 2 + close + 2;
            continue;
        }
        // Copy one char (respecting UTF-8 boundaries).
        let ch = template[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Every `{{ path }}` placeholder referenced in `template`, trimmed, in order
/// of appearance and **not deduplicated** (a caller that cares about repeats -
/// a template that mentions `{{title}}` twice must not yield two `title`
/// entries - dedupes itself). Runs the same brace-and-`}}`-close scan
/// [`substitute`] does, just to collect the *names* referenced instead of
/// substituting their values. A stray unmatched `{{` (no closing `}}`)
/// contributes nothing, the same as `substitute` leaves such text untouched
/// rather than erroring.
///
/// **It has no caller in the tree today.** Its one consumer was
/// `schema::item_schema_for_screen`, which turned a template's placeholders
/// into an inferred row shape and was deleted in R7 once `<List<LibraryItem>>`
/// let a screen DECLARE that shape instead. This is kept - rather than deleted
/// with it - because P2 named exactly one legitimate use for the scan: a
/// one-shot editor ACTION ("create a shape from this screen's placeholders")
/// that writes a TS `interface` into the source for the user to own. The
/// difference between that and what was removed is not the scan; it is that
/// its output becomes a declaration the author can see and edit, instead of an
/// answer recomputed behind them on every read.
pub fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = template.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'{'
            && let Some(close) = template[i + 2..].find("}}")
        {
            out.push(template[i + 2..i + 2 + close].trim().to_string());
            i = i + 2 + close + 2;
            continue;
        }
        // Copy one char (respecting UTF-8 boundaries), same as `substitute`.
        let ch = template[i..].chars().next().unwrap();
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn lookup_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: BTreeMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |path: &str| map.get(path).cloned()
    }

    #[test]
    fn substitutes_placeholders() {
        let f = lookup_from(&[("name", "Ada"), ("count", "3")]);
        assert_eq!(substitute("Hi {{ name }}, x{{count}}", &f), "Hi Ada, x3");
        assert_eq!(substitute("no braces here", &f), "no braces here");
        assert_eq!(substitute("{{missing}}!", &f), "!");
    }

    /// The half of the unification that had two answers: the path is the WHOLE
    /// trimmed text, so a dotted placeholder asks for the dotted name. The
    /// losing implementation asked for `user` here.
    #[test]
    fn a_dotted_path_is_looked_up_whole() {
        let f = lookup_from(&[("user.email", "ada@example.com"), ("user", "Ada")]);
        assert_eq!(substitute("{{user.email}}", &f), "ada@example.com");
        assert_eq!(substitute("{{ user.email }}", &f), "ada@example.com");
        assert_eq!(substitute("{{user}}", &f), "Ada");
    }

    #[test]
    fn substitution_is_deterministic() {
        let f = lookup_from(&[("a", "1")]);
        assert_eq!(substitute("{{a}}{{a}}", &f), substitute("{{a}}{{a}}", &f));
    }

    #[test]
    fn placeholders_extracts_trimmed_paths_in_order_without_deduplicating() {
        assert_eq!(placeholders("{{title}}"), vec!["title".to_string()]);
        assert_eq!(
            placeholders("{{ title }} by {{author}} ({{title}})"),
            vec!["title".to_string(), "author".to_string(), "title".to_string()],
            "repeats are preserved - dedup is the caller's job"
        );
        assert_eq!(placeholders("no braces here"), Vec::<String>::new());
        // An unmatched open brace (no closing `}}`) is stray text, not a
        // placeholder - same as `substitute` leaves it untouched.
        assert_eq!(placeholders("{{unclosed"), Vec::<String>::new());
    }

    #[test]
    fn placeholders_agrees_with_what_substitute_actually_looks_up() {
        // Two independent scanners answering "what counts as a placeholder"
        // differently would be a silent drift risk - this pins them together
        // without making `placeholders` reuse `substitute`'s internals (a
        // lookup-driven builder and a names-only collector have different
        // enough shapes that sharing an implementation would cost more
        // clarity than it saves).
        use std::cell::RefCell;
        let seen: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let template = "{{title}} by {{author}} ({{title}})";
        let lookup = |p: &str| {
            seen.borrow_mut().push(p.to_string());
            Some(String::new())
        };
        let _ = substitute(template, &lookup);
        assert_eq!(placeholders(template), seen.into_inner());
    }
}
