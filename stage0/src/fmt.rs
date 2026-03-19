/// nuvc fmt — Canonical pretty-printer for Nuvola source
///
/// Walks the AST and emits canonical, consistently-indented Nuvola code.
/// Note: comments are not stored in the AST and are therefore not preserved.

use crate::ast::*;

const INDENT: &str = "  ";

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

pub fn format_program(program: &Program) -> String {
    let mut out = String::new();
    for (i, stmt) in program.iter().enumerate() {
        let s = fmt_stmt(stmt, 0);
        if i > 0 {
            // Blank line before top-level fn/type declarations for readability
            match stmt {
                Stmt::FnDecl(_) | Stmt::AsyncFnDecl(_)
                | Stmt::TypeDecl { .. } | Stmt::TraitDecl { .. }
                | Stmt::ImplDecl { .. } => out.push('\n'),
                _ => {}
            }
        }
        out.push_str(&s);
        out.push('\n');
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// Statements
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_stmt(stmt: &Stmt, depth: usize) -> String {
    let pad = INDENT.repeat(depth);
    match stmt {
        Stmt::Let { name, type_ann, mutable, value } => {
            let op = if *mutable { "=" } else { ":=" };
            let ann = match type_ann {
                Some(t) => format!(": {}", fmt_type(t)),
                None    => String::new(),
            };
            format!("{}{}{} {} {}", pad, name, ann, op, fmt_expr(value, depth))
        }

        Stmt::Destructure { names, value } => {
            format!("{}({}) := {}", pad, names.join(", "), fmt_expr(value, depth))
        }

        Stmt::Assign { target, value } => {
            format!("{}{} = {}", pad, fmt_assign_target(target, depth), fmt_expr(value, depth))
        }

        Stmt::CompoundAssign { target, op, value } => {
            let op_str = match op {
                CompoundOp::Add => "+=",
                CompoundOp::Sub => "-=",
                CompoundOp::Mul => "*=",
                CompoundOp::Div => "/=",
            };
            format!("{}{} {} {}", pad, fmt_assign_target(target, depth), op_str, fmt_expr(value, depth))
        }

        Stmt::FnDecl(def) => fmt_fn_decl(def, depth, false),

        Stmt::AsyncFnDecl(def) => fmt_fn_decl(def, depth, true),

        Stmt::If { cond, then_body, elif_clauses, else_body } => {
            let mut s = format!("{}if {}", pad, fmt_expr(cond, depth));
            s.push_str(&fmt_block(then_body, depth, true));
            for (ec, eb) in elif_clauses {
                s.push_str(&format!("\n{}elif {}", pad, fmt_expr(ec, depth)));
                s.push_str(&fmt_block(eb, depth, true));
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("\n{}else", pad));
                s.push_str(&fmt_block(eb, depth, true));
            }
            s
        }

        Stmt::For { var, iter, body } => {
            let var_str = match var {
                ForVar::Simple(v)  => v.clone(),
                ForVar::Tuple(vs)  => format!("({})", vs.join(", ")),
            };
            let mut s = format!("{}for {} in {}", pad, var_str, fmt_expr(iter, depth));
            s.push_str(&fmt_block(body, depth, false));
            s
        }

        Stmt::While { cond, body } => {
            let mut s = format!("{}while {}", pad, fmt_expr(cond, depth));
            s.push_str(&fmt_block(body, depth, false));
            s
        }

        Stmt::Match { expr, arms } => fmt_match_stmt(&pad, expr, arms, depth),

        Stmt::Return(None)    => format!("{}return", pad),
        Stmt::Return(Some(e)) => format!("{}return {}", pad, fmt_expr(e, depth)),
        Stmt::Break(None)     => format!("{}break", pad),
        Stmt::Break(Some(e))  => format!("{}break {}", pad, fmt_expr(e, depth)),
        Stmt::Continue        => format!("{}continue", pad),

        Stmt::TypeDecl { name, kind } => fmt_type_decl(&pad, name, kind, depth),

        Stmt::TraitDecl { name, methods } => {
            let mut s = format!("{}trait {}", pad, name);
            for m in methods {
                s.push('\n');
                s.push_str(&fmt_fn_decl(m, depth + 1, false));
            }
            s
        }

        Stmt::ImplDecl { trait_name, type_name, methods } => {
            let header = match trait_name {
                Some(tr) => format!("{}impl {} for {}", pad, tr, type_name),
                None     => format!("{}impl {}", pad, type_name),
            };
            let mut s = header;
            for m in methods {
                s.push('\n');
                s.push_str(&fmt_fn_decl(m, depth + 1, false));
            }
            s
        }

        Stmt::Comptime { name, value } => {
            format!("{}comptime {} := {}", pad, name, fmt_expr(value, depth))
        }

        Stmt::ExternFn { lib, name, params, ret_type } => {
            let lib_str = lib.as_ref().map(|l| format!("\"{}\" ", l)).unwrap_or_default();
            let params_str = fmt_params(params);
            let ret_str = ret_type.as_ref().map(|t| format!(" -> {}", fmt_type(t))).unwrap_or_default();
            format!("{}extern {}fn {}({}){}", pad, lib_str, name, params_str, ret_str)
        }

        Stmt::Unsafe(body) => {
            let mut s = format!("{}unsafe", pad);
            s.push_str(&fmt_block(body, depth, false));
            s
        }

        Stmt::AwaitStmt(e) => format!("{}await {}", pad, fmt_expr(e, depth)),
        Stmt::SpawnStmt(e) => format!("{}spawn {}", pad, fmt_expr(e, depth)),
        Stmt::Throw(e) => format!("{}throw {}", pad, fmt_expr(e, depth)),
        Stmt::TryCatch { body, catches } => {
            let mut s = format!("{}try", pad);
            s.push_str(&fmt_block(body, depth, false));
            for (var, handler) in catches {
                s.push_str(&format!("\n{}catch {}", pad, var));
                s.push_str(&fmt_block(handler, depth, false));
            }
            s
        }

        Stmt::Import { path, names, alias } => {
            if path.len() == 1 && path[0].contains('/') {
                // String file import: `import "path/to/file.nvl"`
                let a = alias.as_ref().map(|a| format!(" as {}", a)).unwrap_or_default();
                format!("{}import \"{}\"{}", pad, path[0], a)
            } else {
                let path_str = path.join(".");
                match (names, alias) {
                    (None, None)       => format!("{}import {}", pad, path_str),
                    (None, Some(a))    => format!("{}import {} as {}", pad, path_str, a),
                    (Some(ns), None)   => format!("{}import {}.{{{}}}", pad, path_str, ns.join(", ")),
                    (Some(ns), Some(a))=> format!("{}import {}.{{{}}} as {}", pad, path_str, ns.join(", "), a),
                }
            }
        }

        Stmt::Annotation { name, inner } => {
            format!("{}@{}\n{}", pad, name, fmt_stmt(inner, depth))
        }

        Stmt::Expr(e) => format!("{}{}", pad, fmt_expr(e, depth)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Function declarations
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_fn_decl(def: &FnDef, depth: usize, is_async: bool) -> String {
    let pad    = INDENT.repeat(depth);
    let prefix = if is_async { "async fn" } else { "fn" };

    let generics = if def.generic_params.is_empty() {
        String::new()
    } else {
        let gs: Vec<_> = def.generic_params.iter().map(|gp| {
            if gp.bounds.is_empty() {
                gp.name.clone()
            } else {
                format!("{}: {}", gp.name, gp.bounds.join(" + "))
            }
        }).collect();
        format!("<{}>", gs.join(", "))
    };

    let params_str = fmt_params(&def.params);

    let ret = def.ret_type.as_ref()
        .map(|t| format!(" -> {}", fmt_type(t)))
        .unwrap_or_default();

    let where_str = if def.where_clause.is_empty() {
        String::new()
    } else {
        let ws: Vec<_> = def.where_clause.iter()
            .map(|(n, bs)| format!("{}: {}", n, bs.join(" + ")))
            .collect();
        format!("\n{}  where {}", pad, ws.join(", "))
    };

    // Named vs anonymous: anonymous fns have no name (lambda in stmt position)
    let sig = match &def.name {
        Some(name) => format!("{}{} {}{}({}){}{}", pad, prefix, name, generics, params_str, ret, where_str),
        None       => format!("{}{}{}({}){}{}", pad, prefix, generics, params_str, ret, where_str),
    };

    match &def.body {
        FnBody::Arrow(e)  => format!("{} => {}", sig, fmt_expr(e, depth)),
        FnBody::Block(ss) => {
            let mut s = sig;
            s.push_str(&fmt_block(ss, depth, false));
            s
        }
        FnBody::Abstract  => sig,
    }
}

fn fmt_params(params: &[Param]) -> String {
    params.iter().map(|p| {
        let prefix = if p.variadic { "..." } else { "" };
        let ann = p.type_ann.as_ref().map(|t| format!(": {}", fmt_type(t))).unwrap_or_default();
        let def = p.default.as_ref().map(|d| format!(" = {}", fmt_expr(d, 0))).unwrap_or_default();
        format!("{}{}{}{}", prefix, p.name, ann, def)
    }).collect::<Vec<_>>().join(", ")
}

// ─────────────────────────────────────────────────────────────────────────────
// Blocks
// ─────────────────────────────────────────────────────────────────────────────

/// Format a statement block.
/// `allow_arrow`: if true, a single simple statement may be emitted as `=> stmt`
/// Use false for for/while/unsafe bodies where arrow form may not be supported.
fn fmt_block(body: &[Stmt], depth: usize, allow_arrow: bool) -> String {
    if allow_arrow && body.len() == 1 {
        // Single-statement body — prefer fat-arrow inline if it's an Expr
        if let Stmt::Expr(e) = &body[0] {
            // Only inline simple expressions; blocks with nested structure use indented form
            let s = fmt_expr(e, depth + 1);
            if !s.contains('\n') && s.len() < 60 {
                return format!(" => {}", s);
            }
        }
        if let Stmt::Return(Some(e)) = &body[0] {
            let s = fmt_expr(e, depth + 1);
            if !s.contains('\n') && s.len() < 60 {
                return format!(" => return {}", s);
            }
        }
    }
    let mut s = String::new();
    for stmt in body {
        s.push('\n');
        s.push_str(&fmt_stmt(stmt, depth + 1));
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Match
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_match_stmt(pad: &str, expr: &Expr, arms: &[MatchArm], depth: usize) -> String {
    let mut s = format!("{}match {}", pad, fmt_expr(expr, depth));
    let arm_pad = INDENT.repeat(depth + 1);
    for arm in arms {
        let guard = arm.guard.as_ref()
            .map(|g| format!(" if {}", fmt_expr(g, depth + 1)))
            .unwrap_or_default();
        let pat = fmt_pattern(&arm.pattern);
        match &arm.body {
            MatchBody::Expr(e) => {
                s.push_str(&format!("\n{}{}{} => {}", arm_pad, pat, guard, fmt_expr(e, depth + 1)));
            }
            MatchBody::Block(ss) => {
                s.push_str(&format!("\n{}{}{}", arm_pad, pat, guard));
                s.push_str(&fmt_block(ss, depth + 1, false));
            }
        }
    }
    s
}

// ─────────────────────────────────────────────────────────────────────────────
// Expressions
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_expr(expr: &Expr, depth: usize) -> String {
    match expr {
        Expr::Int(n)   => n.to_string(),
        Expr::Float(f) => {
            // Canonical: always has decimal point
            let s = format!("{}", f);
            if s.contains('.') || s.contains('e') { s } else { format!("{}.0", s) }
        }
        Expr::Str(s)   => {
            // Escape special characters
            let escaped = s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
            format!("\"{}\"", escaped)
        }
        Expr::Bool(b)  => if *b { "true".into() } else { "false".into() },
        Expr::Nil      => "nil".into(),
        Expr::Self_    => "self".into(),
        Expr::Ident(n) => n.clone(),

        Expr::BinOp { op, lhs, rhs } => {
            let op_str = fmt_binop(op);
            let l = fmt_expr_parens(lhs, op, true, depth);
            let r = fmt_expr_parens(rhs, op, false, depth);
            format!("{} {} {}", l, op_str, r)
        }

        Expr::UnOp { op, expr } => {
            match op {
                UnOp::Neg => format!("-{}", fmt_expr_atom(expr, depth)),
                UnOp::Not => format!("not {}", fmt_expr_atom(expr, depth)),
            }
        }

        Expr::Pipe { lhs, rhs } => {
            format!("{} |> {}", fmt_expr(lhs, depth), fmt_expr(rhs, depth))
        }

        Expr::Range { start, end, inclusive } => {
            let sep = if *inclusive { "..=" } else { ".." };
            format!("{}{}{}", fmt_expr(start, depth), sep, fmt_expr(end, depth))
        }

        Expr::Call { callee, args, kwargs } => {
            let mut parts: Vec<String> = args.iter().map(|a| fmt_expr(a, depth)).collect();
            for (k, v) in kwargs {
                parts.push(format!("{} = {}", k, fmt_expr(v, depth)));
            }
            format!("{}({})", fmt_expr(callee, depth), parts.join(", "))
        }

        Expr::MethodCall { obj, method, args, kwargs } => {
            let mut parts: Vec<String> = args.iter().map(|a| fmt_expr(a, depth)).collect();
            for (k, v) in kwargs {
                parts.push(format!("{} = {}", k, fmt_expr(v, depth)));
            }
            format!("{}.{}({})", fmt_expr_atom(obj, depth), method, parts.join(", "))
        }

        Expr::Index { obj, idx } => {
            format!("{}[{}]", fmt_expr_atom(obj, depth), fmt_expr(idx, depth))
        }

        Expr::Field { obj, field } => {
            format!("{}.{}", fmt_expr_atom(obj, depth), field)
        }

        Expr::OptChain { obj, field } => {
            format!("{}?.{}", fmt_expr_atom(obj, depth), field)
        }

        Expr::List(elems) => {
            if elems.is_empty() {
                "[]".into()
            } else {
                let items: Vec<_> = elems.iter().map(|e| fmt_expr(e, depth)).collect();
                format!("[{}]", items.join(", "))
            }
        }

        Expr::Map(pairs) => {
            if pairs.is_empty() {
                "{}".into()
            } else {
                let items: Vec<_> = pairs.iter()
                    .map(|(k, v)| format!("{}: {}", fmt_expr(k, depth), fmt_expr(v, depth)))
                    .collect();
                format!("{{{}}}", items.join(", "))
            }
        }

        Expr::Set(elems) => {
            let items: Vec<_> = elems.iter().map(|e| fmt_expr(e, depth)).collect();
            format!("set({})", items.join(", "))
        }

        Expr::Tuple(elems) => {
            let items: Vec<_> = elems.iter().map(|e| fmt_expr(e, depth)).collect();
            format!("({})", items.join(", "))
        }

        Expr::Struct { name, fields } => {
            let items: Vec<_> = fields.iter().map(|f| match f {
                StructField::Named { name, value } => format!("{}: {}", name, fmt_expr(value, depth)),
                StructField::Spread(e)             => format!("..{}", fmt_expr(e, depth)),
            }).collect();
            format!("{} {{{}}}", name, items.join(", "))
        }

        Expr::Lambda(def) => {
            let params_str = fmt_params(&def.params);
            let ret = def.ret_type.as_ref()
                .map(|t| format!(" -> {}", fmt_type(t)))
                .unwrap_or_default();
            match &def.body {
                FnBody::Arrow(e)  => format!("fn({}){}=> {}", params_str, ret, fmt_expr(e, depth)),
                FnBody::Block(ss) => {
                    let mut s = format!("fn({}){}", params_str, ret);
                    s.push_str(&fmt_block(ss, depth, false));
                    s
                }
                FnBody::Abstract  => format!("fn({}){}", params_str, ret),
            }
        }

        Expr::Placeholder(None) => "_".into(),
        Expr::Placeholder(Some(op)) => match op.as_ref() {
            PlaceholderOp::Bin(binop, rhs) => format!("_ {} {}", fmt_binop(binop), fmt_expr(rhs, depth)),
            PlaceholderOp::Field(f)        => format!("_.{}", f),
        },

        Expr::If { cond, then_expr, elif_clauses, else_expr } => {
            let mut s = format!("if {} => {}", fmt_expr(cond, depth), fmt_expr(then_expr, depth));
            for (ec, ee) in elif_clauses {
                s.push_str(&format!(" else if {} => {}", fmt_expr(ec, depth), fmt_expr(ee, depth)));
            }
            if let Some(ee) = else_expr {
                s.push_str(&format!(" else {}", fmt_expr(ee, depth)));
            }
            s
        }

        Expr::Match { expr, arms } => {
            fmt_match_stmt("", expr, arms, depth)
        }

        Expr::Await(e) => format!("await {}", fmt_expr(e, depth)),
        Expr::Spawn(e) => format!("spawn {}", fmt_expr(e, depth)),
        Expr::Unsafe(ss) => {
            let mut s = "unsafe".to_string();
            s.push_str(&fmt_block(ss, depth, false));
            s
        }
    }
}

/// Wrap expression in parens if needed for operator precedence clarity.
fn fmt_expr_parens(expr: &Expr, parent_op: &BinOp, is_lhs: bool, depth: usize) -> String {
    let needs_parens = match expr {
        Expr::BinOp { op, .. } => {
            let lower_prec = precedence(op) < precedence(parent_op);
            // When RHS has same precedence as parent, always add parens.
            // This preserves original grouping for tensor arithmetic
            // (nv_div does not dispatch to tensor ops, so a*(b/c) != (a*b)/c).
            let same_prec_rhs = !is_lhs && precedence(op) == precedence(parent_op);
            lower_prec || same_prec_rhs
        }
        _ => false,
    };
    let s = fmt_expr(expr, depth);
    if needs_parens { format!("({})", s) } else { s }
}

/// Wrap in parens when used as callee/object to avoid ambiguity.
fn fmt_expr_atom(expr: &Expr, depth: usize) -> String {
    match expr {
        Expr::BinOp { .. } | Expr::UnOp { .. } | Expr::Pipe { .. }
        | Expr::Lambda(_) | Expr::If { .. } | Expr::Match { .. } => {
            format!("({})", fmt_expr(expr, depth))
        }
        _ => fmt_expr(expr, depth),
    }
}

fn precedence(op: &BinOp) -> u8 {
    match op {
        BinOp::Or              => 1,
        BinOp::And             => 2,
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::Is => 3,
        BinOp::Add | BinOp::Sub => 4,
        BinOp::Mul | BinOp::Div | BinOp::IntDiv | BinOp::Mod => 5,
        BinOp::Pow | BinOp::Matmul => 6,
    }
}

fn is_left_assoc(op: &BinOp) -> bool {
    !matches!(op, BinOp::Pow)
}

fn fmt_binop(op: &BinOp) -> &'static str {
    match op {
        BinOp::Add    => "+",
        BinOp::Sub    => "-",
        BinOp::Mul    => "*",
        BinOp::Div    => "/",
        BinOp::IntDiv => "//",
        BinOp::Mod    => "%",
        BinOp::Pow    => "**",
        BinOp::Eq     => "==",
        BinOp::Ne     => "!=",
        BinOp::Lt     => "<",
        BinOp::Gt     => ">",
        BinOp::Le     => "<=",
        BinOp::Ge     => ">=",
        BinOp::And    => "and",
        BinOp::Or     => "or",
        BinOp::Is     => "is",
        BinOp::Matmul => "@",
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Patterns
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_pattern(pat: &Pattern) -> String {
    match pat {
        Pattern::Wildcard           => "_".into(),
        Pattern::Literal(e)         => fmt_expr(e, 0),
        Pattern::NegInt(n)          => format!("-{}", n),
        Pattern::Range { start, end, inclusive } => {
            let sep = if *inclusive { "..=" } else { ".." };
            match end {
                Some(e) => format!("{}{}{}", start, sep, e),
                None    => format!("{}{}", start, sep),
            }
        }
        Pattern::SomePat(p)         => format!("Some({})", fmt_pattern(p)),
        Pattern::NonePat            => "None".into(),
        Pattern::OkPat(p)           => format!("Ok({})", fmt_pattern(p)),
        Pattern::ErrPat(p)          => format!("Err({})", fmt_pattern(p)),
        Pattern::Ctor { name, variant, args } => {
            let base = match variant {
                Some(v) => format!("{}.{}", name, v),
                None    => name.clone(),
            };
            if args.is_empty() {
                base
            } else {
                let ps: Vec<_> = args.iter().map(fmt_pattern).collect();
                format!("{}({})", base, ps.join(", "))
            }
        }
        Pattern::Bind(n)            => n.clone(),
        Pattern::Or(pats)           => {
            pats.iter().map(fmt_pattern).collect::<Vec<_>>().join(" | ")
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Type expressions
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_type(t: &TypeExpr) -> String {
    match t {
        TypeExpr::Named(name, args) => {
            if args.is_empty() {
                name.clone()
            } else {
                let ts: Vec<_> = args.iter().map(fmt_type).collect();
                format!("{}<{}>", name, ts.join(", "))
            }
        }
        TypeExpr::Tuple(ts) => {
            let inner: Vec<_> = ts.iter().map(fmt_type).collect();
            format!("({})", inner.join(", "))
        }
        TypeExpr::List(t)   => format!("[{}]", fmt_type(t)),
        TypeExpr::Ref(t)    => format!("&{}", fmt_type(t)),
        TypeExpr::Option(t) => format!("?{}", fmt_type(t)),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Type declarations
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_type_decl(pad: &str, name: &str, kind: &TypeDeclKind, depth: usize) -> String {
    let inner_pad = INDENT.repeat(depth + 1);
    match kind {
        TypeDeclKind::Struct(fields) => {
            let mut s = format!("{}type {}", pad, name);
            for f in fields {
                let def = f.default.as_ref()
                    .map(|d| format!(" = {}", fmt_expr(d, depth + 1)))
                    .unwrap_or_default();
                s.push_str(&format!("\n{}{}: {}{}", inner_pad, f.name, fmt_type(&f.type_ann), def));
            }
            s
        }
        TypeDeclKind::Enum(variants) => {
            let mut s = format!("{}type {}", pad, name);
            for v in variants {
                if v.fields.is_empty() {
                    s.push_str(&format!("\n{}{}", inner_pad, v.name));
                } else {
                    let fs: Vec<_> = v.fields.iter().map(|f| {
                        match &f.name {
                            Some(n) => format!("{}: {}", n, fmt_type(&f.type_ann)),
                            None    => fmt_type(&f.type_ann),
                        }
                    }).collect();
                    s.push_str(&format!("\n{}{}({})", inner_pad, v.name, fs.join(", ")));
                }
            }
            s
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Assignment targets
// ─────────────────────────────────────────────────────────────────────────────

fn fmt_assign_target(t: &AssignTarget, depth: usize) -> String {
    match t {
        AssignTarget::Ident(n)       => n.clone(),
        AssignTarget::Index { obj, idx }   => format!("{}[{}]", fmt_expr(obj, depth), fmt_expr(idx, depth)),
        AssignTarget::Field { obj, field } => format!("{}.{}", fmt_expr(obj, depth), field),
    }
}
