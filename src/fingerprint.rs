use std::collections::HashMap;
use std::thread;

use oxc_allocator::Allocator;
use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind, Expression, ModuleExportName, Statement};
use oxc_ast::AstKind;
use oxc_parser::Parser;
use oxc_semantic::{AstNodes, NodeId, Semantic, SemanticBuilder, SymbolFlags, SymbolId};
use oxc_span::SourceType;

const STACK_SIZE: usize = 256 * 1024 * 1024;

struct Sym {
    /// AST node ids from `Program` down to (and including) the declaration node.
    path: Vec<NodeId>,
    kind: &'static str,
    reads: u32,
    writes: u32,
    /// `Some` for a const/var whose initializer is a primitive literal.
    literal: Option<String>,
}

/// Compute a rename-stable "semantic fingerprint" of the module's exported
/// API surface. Only exported symbols are fingerprinted, deeply (symbols
/// declared inside an export's subtree are included). Minified renames of
/// unexported names therefore never change the fingerprint.
pub fn fingerprint(source: &str) -> Result<String, String> {
    let src = source.to_string();
    let handle = thread::Builder::new()
        .stack_size(STACK_SIZE)
        .name("fingerprint".into())
        .spawn(move || fingerprint_inner(&src))
        .map_err(|e| e.to_string())?;
    handle.join().map_err(|e| format!("fingerprint thread panicked: {e:?}"))?
}

fn fingerprint_inner(source: &str) -> Result<String, String> {
    let allocator = Allocator::default();
    let ret = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if ret.panicked {
        return Err("parser panicked".into());
    }
    if !ret.diagnostics.is_empty() {
        return Err(format!("{} parse diagnostics", ret.diagnostics.len()));
    }
    let program = ret.program;
    let semantic = SemanticBuilder::new().with_build_nodes(true).build(&program).semantic;
    build_fingerprint(&semantic, program.body.as_slice())
}

fn build_fingerprint(semantic: &Semantic, body: &[Statement]) -> Result<String, String> {
    let scoping = semantic.scoping();
    let nodes = semantic.nodes();
    let root = scoping.root_scope_id();

    let mut syms: Vec<Sym> = Vec::new();
    let mut root_symbols: HashMap<&str, SymbolId> = HashMap::new();

    for sid in scoping.symbol_ids() {
        let flags = scoping.symbol_flags(sid);
        let name = scoping.symbol_name(sid);

        if scoping.symbol_scope_id(sid) == root && !flags.is_import() && !flags.is_type_parameter()
        {
            root_symbols.insert(name, sid);
        }

        let Some(decl) = scoping.symbol_declarations(sid).next() else {
            continue;
        };

        let mut chain = vec![decl];
        loop {
            let cur = *chain.last().unwrap();
            if matches!(nodes.kind(cur), AstKind::Program(_)) {
                break;
            }
            chain.push(nodes.parent_id(cur));
        }
        chain.reverse();

        let (reads, writes) = {
            let mut r = 0u32;
            let mut w = 0u32;
            for reference in semantic.symbol_references(sid) {
                if reference.is_read() {
                    r += 1;
                }
                if reference.is_write() {
                    w += 1;
                }
            }
            (r, w)
        };

        syms.push(Sym {
            kind: kind_of(&flags, &chain, nodes),
            literal: literal_of(&chain, nodes),
            reads,
            writes,
            path: chain,
        });
    }

    let mut exports: Vec<(String, NodeId)> = Vec::new();
    for stmt in body {
        match stmt {
            Statement::ExportDeclaration(ed) => match &ed.declaration {
                Declaration::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        exports.push((id.name.as_str().to_string(), f.node_id.get()));
                    }
                }
                Declaration::ClassDeclaration(c) => {
                    if let Some(id) = &c.id {
                        exports.push((id.name.as_str().to_string(), c.node_id.get()));
                    }
                }
                Declaration::VariableDeclaration(vd) => {
                    for declarator in &vd.declarations {
                        if let Some(name) = declarator.id.get_identifier_name() {
                            exports.push((name.as_str().to_string(), declarator.node_id.get()));
                        }
                    }
                }
                _ => {}
            },
            Statement::ExportNamedDeclaration(nd) => {
                for spec in &nd.specifiers {
                    let local = module_export_name(&spec.local);
                    let exported = module_export_name(&spec.exported);
                    if let Some(&sid) = root_symbols.get(local.as_str()) {
                        if let Some(decl) = scoping.symbol_declarations(sid).next() {
                            exports.push((exported, decl));
                        }
                    }
                }
            }
            Statement::ExportDefaultDeclaration(dd) => match &dd.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(f) => {
                    exports.push(("default".into(), f.node_id.get()));
                }
                ExportDefaultDeclarationKind::ClassDeclaration(c) => {
                    exports.push(("default".into(), c.node_id.get()));
                }
                _ => exports.push(("default".into(), dd.node_id.get())),
            },
            _ => {}
        }
    }

    if exports.is_empty() {
        return Ok(String::new());
    }

    let mut out: Vec<(String, String)> = Vec::with_capacity(exports.len());
    for (name, root_node) in &exports {
        let mut entries: Vec<String> = Vec::new();
        for sym in &syms {
            let Some(pos) = sym.path.iter().position(|n| n == root_node) else {
                continue;
            };
            let mut rel = String::new();
            for node in &sym.path[pos + 1..] {
                rel.push_str(kind_tag(nodes.kind(*node)));
                rel.push('/');
            }
            let lit = sym.literal.as_deref().unwrap_or("");
            entries.push(format!("{rel}{}|{}|{}{}", sym.kind, sym.reads, sym.writes, lit));
        }
        entries.sort();
        out.push((name.clone(), entries.join("\n")));
    }
    out.sort();

    let mut fp = String::new();
    for (name, entries_text) in &out {
        fp.push_str(name);
        fp.push('\n');
        fp.push_str(entries_text);
        fp.push_str("\n\n");
    }
    Ok(fp)
}

fn module_export_name(n: &ModuleExportName) -> String {
    match n {
        ModuleExportName::IdentifierName(i) => i.name.as_str().to_string(),
        ModuleExportName::IdentifierReference(i) => i.name.as_str().to_string(),
        ModuleExportName::StringLiteral(s) => s.value.as_str().to_string(),
    }
}

fn kind_of(flags: &SymbolFlags, chain: &[NodeId], nodes: &AstNodes) -> &'static str {
    if flags.is_function() {
        return "function";
    }
    if flags.is_class() {
        return "class";
    }
    if flags.is_const_variable() {
        return "const";
    }
    if flags.is_block_scoped() {
        return "let";
    }
    if flags.is_variable() {
        let is_param = chain.iter().any(|node| {
            matches!(
                nodes.kind(*node),
                AstKind::FormalParameter(_)
                    | AstKind::FormalParameters(_)
                    | AstKind::FormalParameterRest(_)
                    | AstKind::CatchParameter(_)
            )
        });
        return if is_param { "param" } else { "var" };
    }
    "other"
}

/// For a const/var binding whose initializer is a primitive literal, return
/// `Some("~<value>")` so version-number bumps are visible in the fingerprint.
/// Object/array/template initializers are ignored (chunk hashes, debug ids).
fn literal_of(chain: &[NodeId], nodes: &AstNodes) -> Option<String> {
    for node in chain.iter().rev() {
        if let AstKind::VariableDeclarator(vd) = nodes.kind(*node) {
            return match &vd.init {
                Some(Expression::StringLiteral(s)) => Some(format!("~{}", s.value.as_str())),
                Some(Expression::NumericLiteral(n)) => Some(format!("~{}", n.value)),
                Some(Expression::BooleanLiteral(b)) => Some(format!("~{}", b.value)),
                Some(Expression::NullLiteral(_)) => Some("~null".into()),
                _ => None,
            };
        }
    }
    None
}

/// Stable structural name for an AST kind. Only the kinds that can appear on a
/// declaration path are distinguished; everything else collapses to "node"
/// (harmless: identical entries merge in the sorted multiset).
fn kind_tag(kind: AstKind) -> &'static str {
    match kind {
        AstKind::Program(_) => "Program",
        AstKind::BlockStatement(_) => "Block",
        AstKind::VariableDeclaration(_) => "VarDecl",
        AstKind::VariableDeclarator(_) => "VarDeclarator",
        AstKind::Function(_) => "Function",
        AstKind::FormalParameters(_) => "Params",
        AstKind::FormalParameter(_) => "Param",
        AstKind::FunctionBody(_) => "FnBody",
        AstKind::ArrowFunctionExpression(_) => "ArrowFn",
        AstKind::Class(_) => "Class",
        AstKind::ClassBody(_) => "ClassBody",
        AstKind::MethodDefinition(_) => "Method",
        AstKind::PropertyDefinition(_) => "PropDef",
        AstKind::StaticBlock(_) => "StaticBlock",
        AstKind::BindingIdentifier(_) => "BindId",
        AstKind::ExpressionStatement(_) => "ExprStmt",
        AstKind::ReturnStatement(_) => "Return",
        AstKind::IfStatement(_) => "If",
        AstKind::ForStatement(_) => "For",
        AstKind::ForOfStatement(_) => "ForOf",
        AstKind::ForInStatement(_) => "ForIn",
        AstKind::WhileStatement(_) => "While",
        AstKind::DoWhileStatement(_) => "DoWhile",
        AstKind::SwitchStatement(_) => "Switch",
        AstKind::SwitchCase(_) => "Case",
        AstKind::TryStatement(_) => "Try",
        AstKind::CatchClause(_) => "Catch",
        AstKind::ThrowStatement(_) => "Throw",
        AstKind::LabeledStatement(_) => "Label",
        AstKind::BreakStatement(_) => "Break",
        AstKind::ContinueStatement(_) => "Continue",
        AstKind::ObjectExpression(_) => "Object",
        AstKind::ObjectProperty(_) => "ObjProp",
        AstKind::ArrayExpression(_) => "Array",
        AstKind::CallExpression(_) => "Call",
        AstKind::NewExpression(_) => "New",
        AstKind::SequenceExpression(_) => "Seq",
        AstKind::AssignmentExpression(_) => "Assign",
        AstKind::BinaryExpression(_) => "Binary",
        AstKind::LogicalExpression(_) => "Logical",
        AstKind::ConditionalExpression(_) => "Cond",
        AstKind::TemplateLiteral(_) => "Template",
        AstKind::StaticMemberExpression(_) => "StaticMember",
        AstKind::ComputedMemberExpression(_) => "ComputedMember",
        AstKind::ChainExpression(_) => "Chain",
        AstKind::AwaitExpression(_) => "Await",
        AstKind::UnaryExpression(_) => "Unary",
        AstKind::UpdateExpression(_) => "Update",
        AstKind::SpreadElement(_) => "Spread",
        AstKind::ParenthesizedExpression(_) => "Paren",
        AstKind::YieldExpression(_) => "Yield",
        AstKind::ImportDeclaration(_) => "Import",
        AstKind::ExportDeclaration(_) => "ExportDecl",
        AstKind::ExportNamedDeclaration(_) => "ExportNamed",
        AstKind::ExportDefaultDeclaration(_) => "ExportDefault",
        AstKind::ExportFromDeclaration(_) => "ExportFrom",
        AstKind::ExportAllDeclaration(_) => "ExportAll",
        AstKind::ExportSpecifier(_) => "ExportSpec",
        AstKind::ImportSpecifier(_) => "ImportSpec",
        AstKind::ImportDefaultSpecifier(_) => "ImportDefault",
        AstKind::ImportNamespaceSpecifier(_) => "ImportNamespace",
        AstKind::StringLiteral(_) => "String",
        AstKind::NumericLiteral(_) => "Number",
        AstKind::BooleanLiteral(_) => "Bool",
        AstKind::NullLiteral(_) => "Null",
        AstKind::BigIntLiteral(_) => "BigInt",
        AstKind::RegExpLiteral(_) => "RegExp",
        AstKind::IdentifierReference(_) => "IdentRef",
        AstKind::AssignmentPattern(_) => "AssignPat",
        AstKind::ObjectPattern(_) => "ObjectPat",
        AstKind::ArrayPattern(_) => "ArrayPat",
        AstKind::BindingRestElement(_) => "Rest",
        AstKind::CatchParameter(_) => "CatchParam",
        _ => "node",
    }
}
