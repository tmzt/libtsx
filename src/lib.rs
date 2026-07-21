use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

/// A parsed TSX document holding both the allocator arena and the resulting AST.
pub struct ParsedTsx {
    // The allocator must outlive the parsed AST. 
    // We can't easily return a struct holding both in safe Rust if the AST borrows from the allocator
    // directly in the same struct without self-referential lifetimes, so we expose a method that 
    // takes a closure to operate on the AST, or we just provide a parse-and-extract function.
}

/// Helper function to parse a TSX string and extract specific structural data (like MountPoints)
/// without having to leak the allocator or deal with self-referential structs.
pub fn parse_tsx<F, R>(source: &str, f: F) -> Result<R, Vec<String>>
where
    F: FnOnce(&oxc_ast::ast::Program<'_>) -> R,
{
    let allocator = Allocator::default();
    let source_type = SourceType::tsx();
    
    let ret = Parser::new(&allocator, source, source_type).parse();
    
    if ret.diagnostics.is_empty() {
        Ok(f(&ret.program))
    } else {
        Err(ret.diagnostics.into_iter().map(|e| format!("{:?}", e)).collect())
    }
}
