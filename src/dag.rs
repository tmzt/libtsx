//! `DagNode` — the serializable code-graph contract (semantic AST).
//!
//! This is the **single owned boundary type** between the TSX/TypeScript world
//! and everything downstream (nocap-witgen, highbay-build, the language
//! switcher). No `oxc_*` type appears here or anywhere in this module's public
//! API: consumers get plain, serde-serializable Rust data.
//!
//! Scope (deliberately minimal, forward-compatible):
//! * **TS `interface` declarations** — name, fields, optionality, nested
//!   shapes ([`InterfaceDecl`], [`TypeShape`]). These are the Props shapes
//!   that project into WIT records and generated forms.
//! * **Simple event-handler ops** ([`HandlerDecl`], [`Stmt`], [`Expr`]) — the
//!   restricted semantic AST that projects symmetrically across language
//!   views and synthesizes directly to wasm. Complex Modules are *not*
//!   represented here; they are opaque native-language units by design.
//!
//! The parser upgrade that *produces* these values from TSX source is a later
//! phase; this module is types + serde only.

use serde::{Deserialize, Serialize};

/// A node in the code graph. The umbrella type consumed by `nocap-witgen`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DagNode {
    /// A whole module: interfaces + handlers + host imports.
    Module(DagModule),
    /// A single TS `interface` declaration.
    Interface(InterfaceDecl),
    /// A single event-handler function.
    Handler(HandlerDecl),
}

/// A module of code: the unit `highbay-build` feeds to witgen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DagModule {
    /// Module name (used for the WIT package/interface identity).
    pub name: String,
    /// TS `interface` declarations (Props shapes).
    pub interfaces: Vec<InterfaceDecl>,
    /// Host functions the handlers may call (nav edges, actions, nocap ops).
    pub imports: Vec<FuncSig>,
    /// Event handlers (simple semantic-AST bodies).
    pub handlers: Vec<HandlerDecl>,
}

/// A TS `interface` declaration: `interface Name { fields… }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
}

/// An ES `import { a, b as c } from "src"` declaration — the **typed reference**
/// edge from a module to a provider it consumes (a screen importing a Script's
/// exported source, e.g. a `ListAdapter` provider). Owned + serde; no `oxc_*`
/// type appears here. The semantic AST records the edge (what a module depends
/// on and under what local name); resolving it to a concrete provider is the
/// consumer's job (highbay_ui's provider registry today; a real module runtime
/// later).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportDecl {
    /// The module specifier string (`from "…"`) — a Script's display name in the
    /// Highbay module system, or a real package path.
    pub source: String,
    /// The imported bindings, in source order.
    pub names: Vec<ImportName>,
}

/// One imported binding of an [`ImportDecl`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportName {
    /// The local name this module refers to the binding by.
    pub local: String,
    /// The name exported by the source module. Equals `local` for a default or
    /// namespace import; the `imported` half of `imported as local` otherwise.
    pub imported: String,
    /// Which import form introduced the binding.
    pub kind: ImportKind,
}

/// The three import binding forms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImportKind {
    /// `import { imported as local }` (or `import { name }`).
    Named,
    /// `import local from "src"`.
    Default,
    /// `import * as local from "src"`.
    Namespace,
}

/// One field of an interface (or one named function parameter).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldDecl {
    pub name: String,
    pub ty: TypeShape,
    /// TS `name?: T` optionality.
    #[serde(default)]
    pub optional: bool,
}

/// The shape of a type as it crosses the edge. Maps 1:1 onto WIT types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TypeShape {
    Bool,
    /// TS `number` annotated as 32-bit integer.
    S32,
    /// TS `number`/`bigint` annotated as 64-bit integer.
    S64,
    F32,
    /// TS `number` (default lowering).
    F64,
    String,
    /// `T[]` / `Array<T>`.
    List(Box<TypeShape>),
    /// `T | undefined` / optional shapes used positionally.
    Option(Box<TypeShape>),
    /// An inline anonymous object shape (`{ a: number }`); witgen hoists
    /// these into named records.
    Record(Vec<FieldDecl>),
    /// A reference to another interface by name.
    Named(String),
}

/// A function signature (handler export or host import).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FuncSig {
    pub name: String,
    pub params: Vec<FieldDecl>,
    /// `None` = no return value.
    #[serde(default)]
    pub result: Option<TypeShape>,
}

/// An event handler: a signature plus a *simple* body.
///
/// Bodies are restricted to the semantic-AST op set ([`Stmt`]/[`Expr`]) so
/// they can project across language views and synthesize directly to wasm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerDecl {
    pub sig: FuncSig,
    pub body: Vec<Stmt>,
}

/// Simple statement ops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Stmt {
    /// Evaluate an expression for its effect (host calls).
    Expr(Expr),
    /// `return;` / `return expr;`
    Return(Option<Expr>),
    /// `if (cond) { … } else { … }`
    If {
        cond: Expr,
        then_branch: Vec<Stmt>,
        #[serde(default)]
        else_branch: Vec<Stmt>,
    },
    /// Property write through the flat nocap ABI (`setProperty`).
    Set { path: String, value: Expr },
}

/// Simple expression ops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expr {
    LitBool(bool),
    LitS32(i32),
    LitS64(i64),
    LitF32(f32),
    LitF64(f64),
    LitStr(String),
    /// Reference to a handler parameter by index.
    Param(u32),
    /// Property read through the flat nocap ABI (`getProperty`).
    Get { path: String },
    /// Binary operation.
    Bin {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    /// Call a declared host import (nav edge, action trigger, …).
    Call { callee: String, args: Vec<Expr> },
}

/// Binary operators available to simple handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// Logical and — note: synthesized non-short-circuit (both sides eval).
    And,
    /// Logical or — note: synthesized non-short-circuit (both sides eval).
    Or,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_module() -> DagModule {
        DagModule {
            name: "counter".into(),
            interfaces: vec![InterfaceDecl {
                name: "CounterProps".into(),
                fields: vec![
                    FieldDecl { name: "label".into(), ty: TypeShape::String, optional: false },
                    FieldDecl { name: "count".into(), ty: TypeShape::S32, optional: false },
                    FieldDecl {
                        name: "history".into(),
                        ty: TypeShape::List(Box::new(TypeShape::S32)),
                        optional: true,
                    },
                    FieldDecl {
                        name: "style".into(),
                        ty: TypeShape::Record(vec![FieldDecl {
                            name: "bold".into(),
                            ty: TypeShape::Bool,
                            optional: false,
                        }]),
                        optional: false,
                    },
                ],
            }],
            imports: vec![FuncSig {
                name: "navigate".into(),
                params: vec![FieldDecl { name: "target".into(), ty: TypeShape::S32, optional: false }],
                result: None,
            }],
            handlers: vec![HandlerDecl {
                sig: FuncSig {
                    name: "onIncrement".into(),
                    params: vec![FieldDecl {
                        name: "step".into(),
                        ty: TypeShape::S32,
                        optional: false,
                    }],
                    result: Some(TypeShape::S32),
                },
                body: vec![Stmt::Return(Some(Expr::Bin {
                    op: BinOp::Add,
                    lhs: Box::new(Expr::Param(0)),
                    rhs: Box::new(Expr::LitS32(1)),
                }))],
            }],
        }
    }

    #[test]
    fn dag_round_trips_through_serde() {
        let node = DagNode::Module(sample_module());
        let json = serde_json::to_string(&node).expect("serialize");
        let back: DagNode = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
    }

    #[test]
    fn import_decl_round_trips_through_serde() {
        let imp = ImportDecl {
            source: "Library Feed".into(),
            names: vec![
                ImportName { local: "libraryFeed".into(), imported: "libraryFeed".into(), kind: ImportKind::Named },
                ImportName { local: "Feed".into(), imported: "default".into(), kind: ImportKind::Default },
            ],
        };
        let json = serde_json::to_string(&imp).expect("serialize");
        let back: ImportDecl = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(imp, back);
    }

    #[test]
    fn optional_fields_default_when_absent() {
        // Forward-compat: older serializations without `optional`/`result`
        // still deserialize.
        let json = r#"{ "name": "x", "ty": "Bool" }"#;
        let field: FieldDecl = serde_json::from_str(json).expect("deserialize");
        assert!(!field.optional);
        let json = r#"{ "name": "f", "params": [] }"#;
        let sig: FuncSig = serde_json::from_str(json).expect("deserialize");
        assert_eq!(sig.result, None);
    }
}
