//! Public entry point for the formatter — wires the tokenizer to the printer.

use anyhow::Result;

use super::{printer, tokenizer};

/// Format a Handlebars template source. See [crate-level docs](super)
/// for the rule set.
///
/// # Errors
///
/// Returns an error if tokenization or pretty-printing fails (typically a
/// malformed template that the tokenizer can't recover from).
pub fn format(src: &str) -> Result<String> {
    let tokens = tokenizer::tokenize(src)?;
    printer::print(&tokens)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;
    use crate::fmt::{
        printer::renders_as_bare_boolean,
        tokenizer::{Attr, Token, parse_attributes, tokenize},
    };

    // ── generated template fragments ──────────────────────────────────────
    //
    // `any::<String>()` practically never produces something a Handlebars
    // tokenizer recognises, so a property asserted only over it is asserted
    // over pass-through garbage. These strategies build syntactically valid
    // fragments — balanced tags, balanced block helpers, real attribute
    // shapes — so the printer's actual rules get exercised.

    /// Container tags whose body the printer re-indents. Raw-content tags
    /// (`script` / `style` / `pre` / `textarea`) are deliberately absent:
    /// their bodies pass through verbatim under a separate rule set and are
    /// covered by the printer's own fixtures.
    const CONTAINER_TAGS: &[&str] = &["div", "p", "span", "li", "section", "label", "a", "button"];

    /// Void tags — the printer must self-close every one of them.
    const VOID_TAGS: &[&str] = &["input", "br", "meta", "img", "hr"];

    /// Attributes with no boolean meaning: the value is always kept.
    const VALUE_ATTRS: &[&str] = &["class", "id", "href", "type", "name", "data-x", "hx-get"];

    /// Attributes whose redundant value collapses to the bare name.
    const BOOLEAN_ATTRS: &[&str] = &["required", "disabled", "checked", "selected", "readonly"];

    /// Attribute values. None contains `"` or `'`: the printer entity-encodes
    /// a bare `"` so the value it emits can always be re-parsed, and the
    /// content signature compares attribute values verbatim. Generating one
    /// would assert that a deliberate, documented rewrite is a content change.
    /// The rewrite has its own fixture in `printer.rs`.
    const ATTR_VALUES: &[&str] = &[
        "",
        "btn",
        "btn primary",
        "/admin/collections",
        "#anchor",
        "field-1",
        " leading",
        "trailing ",
        "/admin/{{ id }}",
        "{{{json row}}}",
    ];

    /// Text runs. No `<`, `{` or `}` — those open a tag or a mustache rather
    /// than text.
    const TEXT_RUNS: &[&str] = &[
        "Hello",
        "Save changes",
        "a   b",
        "  padded  ",
        "x",
        "Titel & Wert",
        "yes / no",
        "ümlaut … ok",
    ];

    /// Mustaches in body position, including a triple-stash and the `t`
    /// helper with a quoted key.
    const MUSTACHES: &[&str] = &[
        "{{x}}",
        "{{ doc.title }}",
        "{{t \"save\"}}",
        "{{{json doc}}}",
        "{{@index}}",
    ];

    /// Comments — preserved verbatim, in both syntaxes.
    const COMMENTS: &[&str] = &[
        "<!-- note -->",
        "<!--x-->",
        "{{!-- doc --}}",
        "{{! short }}",
    ];

    /// Block helpers as `(open, close)` pairs. Each owns a line and indents
    /// its body one level.
    const BLOCK_HELPERS: &[(&str, &str)] = &[
        ("{{#if flag}}", "{{/if}}"),
        ("{{#unless flag}}", "{{/unless}}"),
        ("{{#each items}}", "{{/each}}"),
        ("{{#> partials/field}}", "{{/partials/field}}"),
    ];

    /// Pick one of a fixed set of literals.
    fn one_of(values: &'static [&'static str]) -> impl Strategy<Value = String> {
        prop::sample::select(values).prop_map(str::to_owned)
    }

    /// One attribute, in every shape the attribute rules have to handle: a
    /// name/value pair in either quote style, a bare boolean, a boolean
    /// carrying a redundant value, a bare mustache, and an embedded
    /// `{{#if}}` block.
    fn attribute() -> impl Strategy<Value = String> {
        prop_oneof![
            (one_of(VALUE_ATTRS), one_of(ATTR_VALUES))
                .prop_map(|(name, value)| quoted_attr(&name, "\"", &value)),
            (one_of(VALUE_ATTRS), one_of(ATTR_VALUES))
                .prop_map(|(name, value)| quoted_attr(&name, "'", &value)),
            one_of(BOOLEAN_ATTRS),
            one_of(BOOLEAN_ATTRS).prop_map(|name| quoted_attr(&name, "\"", &name)),
            Just("{{attrs}}".to_owned()),
            Just("{{#if flag}}required{{/if}}".to_owned()),
        ]
    }

    /// `name=<quote>value<quote>`. The printer picks the quote itself, so
    /// both styles must appear in the generated source.
    fn quoted_attr(name: &str, quote: &str, value: &str) -> String {
        [name, "=", quote, value, quote].concat()
    }

    /// Zero to three attributes — spanning both the inline (0–1) and the
    /// stacked (2+) branch of the attribute-list rule.
    fn attribute_list() -> impl Strategy<Value = String> {
        prop::collection::vec(attribute(), 0..4).prop_map(|attrs| attrs.join(" "))
    }

    /// `<tag attrs…` plus `close` — the one place the optional space
    /// between the tag name and a non-empty attribute list is decided.
    fn open_tag(tag: &str, attrs: &str, close: &str) -> String {
        let separator = if attrs.is_empty() { "" } else { " " };

        ["<", tag, separator, attrs, close].concat()
    }

    fn container_element(tag: &str, attrs: &str, body: &str) -> String {
        let open = open_tag(tag, attrs, ">");

        [open.as_str(), body, "</", tag, ">"].concat()
    }

    /// A void element, written both ways round: already self-closed and
    /// left open for the printer to close.
    fn void_element() -> impl Strategy<Value = String> {
        (one_of(VOID_TAGS), attribute_list(), any::<bool>()).prop_map(
            |(tag, attrs, self_closed)| {
                let close = if self_closed { " />" } else { ">" };

                open_tag(&tag, &attrs, close)
            },
        )
    }

    /// A block body: zero or more fragments, one per line.
    fn fragment_body(inner: BoxedStrategy<String>) -> impl Strategy<Value = String> {
        prop::collection::vec(inner, 0..3).prop_map(|parts| parts.join("\n"))
    }

    /// One template fragment: a leaf (text, mustache, comment, void
    /// element) or a container element / block helper wrapping a body of
    /// further fragments. Every branch stays balanced, so the formatter is
    /// exercised on real input rather than on its parse-error paths.
    fn template_fragment() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            one_of(TEXT_RUNS),
            one_of(MUSTACHES),
            one_of(COMMENTS),
            void_element(),
        ];

        leaf.prop_recursive(3, 32, 3, |inner| {
            prop_oneof![
                (
                    one_of(CONTAINER_TAGS),
                    attribute_list(),
                    fragment_body(inner.clone())
                )
                    .prop_map(|(tag, attrs, body)| container_element(&tag, &attrs, &body)),
                (
                    prop::sample::select(BLOCK_HELPERS),
                    fragment_body(inner.clone())
                )
                    .prop_map(|((open, close), body)| [open, body.as_str(), close].concat()),
                (fragment_body(inner.clone()), fragment_body(inner)).prop_map(|(then, other)| {
                    [
                        "{{#if flag}}",
                        then.as_str(),
                        "{{else}}",
                        other.as_str(),
                        "{{/if}}",
                    ]
                    .concat()
                }),
            ]
        })
    }

    /// A whole template file: a few fragments, one per line, newline
    /// terminated the way a file on disk is.
    fn template_source() -> impl Strategy<Value = String> {
        prop::collection::vec(template_fragment(), 0..3).prop_map(|parts| parts.join("\n") + "\n")
    }

    proptest! {
        /// Property: the formatter never panics on arbitrary input — a
        /// malformed template must surface as `Err`, never a crash (it runs in
        /// the pre-commit hook and CI on whatever is on disk).
        #[test]
        fn format_never_panics_on_arbitrary_input(s in any::<String>()) {
            let _ = format(&s);
        }

        /// Property: formatting is idempotent — once a template formats
        /// successfully, re-formatting its output is a fixed point. A
        /// non-idempotent formatter would thrash `fmt --check` in CI.
        #[test]
        fn format_is_idempotent_on_success(s in any::<String>()) {
            if let Ok(once) = format(&s) {
                let twice = format(&once).expect("formatted output must re-format");
                prop_assert_eq!(once, twice);
            }
        }

        /// The same fixed-point property, asserted on input that actually
        /// looks like a template — nested elements, stacked attribute
        /// lists, block helpers, `{{else}}` branches — where the indent
        /// stack, the inline-collapse decision and the line limit all do
        /// real work. On `any::<String>()` almost every case is
        /// pass-through text.
        #[test]
        fn format_is_idempotent_on_templates(src in template_source()) {
            let once = format(&src).expect("a generated template must format");
            let twice = format(&once).expect("formatted output must re-format");
            prop_assert_eq!(once, twice);
        }

        /// Property: formatting never adds, drops, or reorders CONTENT — the
        /// text nodes, raw bodies, comments, and mustache expressions come
        /// through unchanged (down to whitespace, which the formatter may move
        /// and mustache-normalisation may trim). Idempotency alone can't catch
        /// a tokenizer bug that stably mangles content; this can.
        #[test]
        fn format_preserves_content(s in any::<String>()) {
            if let Ok(out) = format(&s)
                && let (Some(before), Some(after)) =
                    (content_signature(&s), content_signature(&out))
            {
                prop_assert_eq!(before, after);
            }
        }

        /// The same preservation property on generated templates, where the
        /// signature's tag and attribute fields are actually populated — a
        /// dropped, reordered or rewritten attribute fails here.
        #[test]
        fn format_preserves_content_of_templates(src in template_source()) {
            let out = format(&src).expect("a generated template must format");
            let before = content_signature(&src).expect("source must tokenize");
            let after = content_signature(&out).expect("output must tokenize");

            prop_assert_eq!(before, after);
        }
    }

    /// Delimiter for a structural field in [`content_signature`]. A control
    /// character, so text content can never be mistaken for one.
    const FIELD_SEP: char = '\u{1}';

    /// The content a formatter must preserve: text, raw bodies, comments and
    /// mustache expressions, plus the tag/attribute skeleton — tag names,
    /// attribute names, attribute values, and their order.
    ///
    /// Exactly what the style guide lets the printer rewrite is normalised
    /// away first, and nothing more:
    ///
    /// - whitespace anywhere (the printer re-indents, collapses inline runs,
    ///   and trims line ends),
    /// - tag and attribute name case (the tokenizer lowercases both),
    /// - quote style, and the `/` a void tag gains,
    /// - a redundant boolean-attribute value (`required=""` → `required`),
    ///   decided by the same predicate the printer renders with.
    ///
    /// Returns `None` when either side fails to tokenize or to re-parse an
    /// attribute list — there is no signature to compare in that case.
    fn content_signature(src: &str) -> Option<String> {
        let mut sig = String::new();

        for tok in tokenize(src).ok()? {
            match tok {
                Token::HtmlStart {
                    name, attrs_raw, ..
                } => {
                    // `self_closed` is excluded on purpose: `<br>` becomes
                    // `<br />` without the element itself changing.
                    push_field(&mut sig, '<', &name);
                    for attr in parse_attributes(attrs_raw).ok()? {
                        push_field(&mut sig, '@', &attr_signature(&attr));
                    }
                }
                Token::HtmlEnd { name } => push_field(&mut sig, '/', &name),
                Token::Text(s)
                | Token::RawText(s)
                | Token::RawBlock(s)
                | Token::HtmlComment(s)
                | Token::HbsComment(s)
                | Token::HbsExpr(s)
                | Token::HbsBlockOpen(s)
                | Token::HbsBlockClose(s)
                | Token::HbsElse(s)
                | Token::HbsPartialOpen(s)
                | Token::HbsPartialClose(s) => push_condensed(&mut sig, s),
            }
        }

        Some(sig)
    }

    /// Append a structural field: a delimiter, a one-character kind marker,
    /// and the whitespace-condensed value.
    fn push_field(sig: &mut String, kind: char, value: &str) {
        sig.push(FIELD_SEP);
        sig.push(kind);
        push_condensed(sig, value);
    }

    fn push_condensed(sig: &mut String, value: &str) {
        sig.extend(value.chars().filter(|c| !c.is_whitespace()));
    }

    /// One attribute reduced to what the printer must keep: the lowercased
    /// name, plus the value unless the boolean rule drops it.
    fn attr_signature(attr: &Attr) -> String {
        match attr {
            Attr::Plain { name, value } => match value {
                Some(v) if !renders_as_bare_boolean(name, Some(v.as_str())) => {
                    [name.as_str(), "=", v.as_str()].concat()
                }
                _ => name.clone(),
            },
            Attr::HbsExpr(s) | Attr::HbsBlock(s) => s.clone(),
        }
    }

    /// The formatter is documented as idempotent (CI gates on
    /// `crap-cms fmt --check`): formatting already-formatted output must be a
    /// fixed point. This guards the tokenizer↔printer round-trip end to end.
    #[test]
    fn formatting_is_idempotent() {
        let src = "<div class=\"a\" id=\"b\">\n{{#if x}}\n<span>{{t \"hi\"}}</span>\n{{/if}}\n<input>\n</div>\n";
        let once = format(src).unwrap();
        let twice = format(&once).unwrap();
        assert_eq!(once, twice, "second format pass changed the output");
    }

    #[test]
    fn self_closes_void_elements() {
        let out = format("<input>\n").unwrap();
        assert!(out.contains("<input />"), "got: {out:?}");
    }

    /// The signature must see attribute names and values, or a printer that
    /// silently dropped or rewrote one would still satisfy
    /// `format_preserves_content`.
    #[test]
    fn signature_distinguishes_attribute_changes() {
        let base = content_signature("<a class=\"card\" href=\"/x\">y</a>").unwrap();

        for mutated in [
            "<a href=\"/x\">y</a>",                // attribute dropped
            "<a class=\"crd\" href=\"/x\">y</a>",  // value mangled
            "<a klass=\"card\" href=\"/x\">y</a>", // name mangled
            "<a href=\"/x\" class=\"card\">y</a>", // order swapped
            "<b class=\"card\" href=\"/x\">y</b>", // tag renamed
        ] {
            assert_ne!(
                base,
                content_signature(mutated).unwrap(),
                "signature must distinguish `{mutated}`"
            );
        }
    }

    /// …while still normalising every rewrite the style guide permits, so
    /// the property doesn't fail on legal output.
    #[test]
    fn signature_normalises_permitted_rewrites() {
        let canonical =
            content_signature("<input class=\"a\" required />\n<br />\n{{t \"k\"}}").unwrap();

        for equivalent in [
            "<INPUT CLASS='a' REQUIRED>\n<BR>\n{{t \"k\"}}", // case, quotes, void close
            "<input class=a required=\"\">\n<br>\n{{t \"k\"}}", // unquoted, empty boolean value
            "<input\n  class=\"a\"\n  required=\"required\"\n/>\n<br />\n{{ t \"k\" }}",
        ] {
            assert_eq!(
                canonical,
                content_signature(equivalent).unwrap(),
                "signature must normalise `{equivalent}`"
            );
        }
    }
}
