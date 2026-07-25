//! TS -> JS type-strip transpile (feature `parse`) — the front end of the
//! TypeScript **module runtime** (MODULE_PLAN M1).
//!
//! A Highbay TypeScript *module* (a "Script") is a full native feature unit; to
//! execute it in a QuickJS-class runtime it must first become plain JavaScript.
//! [`transpile_ts`] does exactly that and no more: it parses the TS, runs the
//! oxc **TypeScript transform** (which erases every type annotation, `interface`,
//! `type` alias, `enum`-as-type, etc.), and re-emits JavaScript. It is a
//! type-strip, not a down-leveller — modern JS (template literals, arrow fns,
//! `export`) passes straight through so the emitted module still runs on any
//! current engine.
//!
//! Quarantine (PLAN §4): the oxc pipeline (`oxc_parser` + `oxc_semantic` +
//! `oxc_transformer` + `oxc_codegen`) lives entirely behind this one owned
//! function; no `oxc_*` type appears in the signature.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};

/// Transpile a TypeScript module source to JavaScript, stripping all type
/// syntax. `export`/`import` are preserved (the source is treated as an ES
/// module), so the result can be run as a `.mjs` module by the runtime.
///
/// Returns the emitted JavaScript on success, or the parser's syntax errors
/// (one per entry) on a hard parse failure. Type-level transform diagnostics do
/// not fail the transpile — a valid parse always yields runnable JS.
pub fn transpile_ts(source: &str) -> Result<String, Vec<String>> {
    let allocator = Allocator::default();
    // Treat modules as TypeScript ES modules: `export function foo(x: T) {}`
    // must parse, and the emitted JS keeps the `export` for `.mjs` execution.
    let source_type = SourceType::ts().with_module(true);

    let parsed = Parser::new(&allocator, source, source_type).parse();
    if !parsed.diagnostics.is_empty() {
        return Err(parsed.diagnostics.iter().map(|e| format!("{e:?}")).collect());
    }
    let mut program = parsed.program;

    // Scoping is required by the transformer; build it from the parsed program.
    let scoping = SemanticBuilder::new().build(&program).semantic.into_scoping();

    // Default options target the current engine (no down-levelling); the
    // TypeScript transform still runs because the program's source type is TS,
    // erasing all type nodes.
    let options = TransformOptions::default();
    let _ = Transformer::new(&allocator, Path::new("module.ts"), &options)
        .build_with_scoping(scoping, &mut program);

    Ok(Codegen::new().build(&program).code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_type_annotations_from_an_exported_function() {
        let ts = "export function libraryFeed(index: number): { title: string } {\n\
                  \x20 return { title: `item ${index + 1}` };\n\
                  }\n";
        let js = transpile_ts(ts).expect("valid TS transpiles");
        // Type syntax is gone...
        assert!(!js.contains(": number"), "param type annotation stripped: {js}");
        assert!(!js.contains(": string"), "return field type stripped: {js}");
        // ...but the runnable shape survives.
        assert!(js.contains("function libraryFeed"), "function kept: {js}");
        assert!(js.contains("export"), "export kept for .mjs execution: {js}");
        assert!(js.contains("title"), "object literal kept: {js}");
    }

    #[test]
    fn erases_interface_and_type_declarations() {
        let ts = "interface Row { title: string; subtitle: string }\n\
                  type Id = number;\n\
                  export function feed(i: Id): Row {\n\
                  \x20 return { title: 'a', subtitle: 'b' };\n\
                  }\n";
        let js = transpile_ts(ts).expect("transpiles");
        assert!(!js.contains("interface"), "interface erased: {js}");
        assert!(!js.to_lowercase().contains("type id"), "type alias erased: {js}");
        assert!(js.contains("function feed"), "runtime fn kept: {js}");
    }

    #[test]
    fn plain_javascript_passes_through() {
        let js_in = "export function f(x) { return x * 2; }\n";
        let js = transpile_ts(js_in).expect("JS is valid TS");
        assert!(js.contains("function f"), "{js}");
        assert!(js.contains("* 2") || js.contains("*2"), "body preserved: {js}");
    }

    #[test]
    fn a_syntax_error_reports_rather_than_panicking() {
        let bad = "export function ( { : : : ";
        assert!(transpile_ts(bad).is_err(), "malformed source is a reported error");
    }
}
