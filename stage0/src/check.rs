/// check.rs — Nuvola static analysis pass (M19)
///
/// Performs a pre-flight check on the AST without compiling.
///
/// Current checks:
///   • Function call arity — user-defined functions called with wrong arg count
///   • Duplicate top-level function names
///
/// Not checked (dynamic language, false positives too high):
///   • Undefined variable references
///   • Type mismatches
///
/// Usage:
///   nuvc --check <file>    → prints diagnostics, exits 0 (clean) or 1 (errors found)

use std::collections::HashMap;
use crate::ast::*;

// ─────────────────────────────────────────────────────────────────────────────
// Diagnostic
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum Level { Error, Warning }

#[derive(Debug, Clone)]
pub struct Diag {
    pub level: Level,
    pub msg:   String,
    /// Context hint, e.g. "in function `foo`" — empty for top-level.
    pub ctx:   String,
}

impl Diag {
    fn error(msg: impl Into<String>, ctx: impl Into<String>) -> Self {
        Diag { level: Level::Error, msg: msg.into(), ctx: ctx.into() }
    }
    fn warning(msg: impl Into<String>, ctx: impl Into<String>) -> Self {
        Diag { level: Level::Warning, msg: msg.into(), ctx: ctx.into() }
    }
}

impl std::fmt::Display for Diag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.level { Level::Error => "error", Level::Warning => "warning" };
        if self.ctx.is_empty() {
            write!(f, "{}: {}", prefix, self.msg)
        } else {
            write!(f, "{}: {} ({})", prefix, self.msg, self.ctx)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FnInfo — what we know about a function definition
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct FnInfo {
    /// Number of required parameters (no default value).
    min_arity: usize,
    /// Total number of parameters (required + optional).
    max_arity: usize,
    /// True if the last param is `...name` (accepts any number of trailing args).
    variadic:  bool,
}

impl FnInfo {
    fn from_params(params: &[Param]) -> Self {
        let variadic = params.last().map(|p| p.variadic).unwrap_or(false);
        let max_arity = params.len();
        let min_arity = params.iter()
            .filter(|p| p.default.is_none() && !p.variadic)
            .count();
        FnInfo { min_arity, max_arity, variadic }
    }

    /// True if `given` positional + keyword args is acceptable.
    fn accepts(&self, given: usize) -> bool {
        if self.variadic { given >= self.min_arity }
        else             { given >= self.min_arity && given <= self.max_arity }
    }

    fn arity_desc(&self) -> String {
        if self.variadic {
            format!("at least {}", self.min_arity)
        } else if self.min_arity == self.max_arity {
            format!("{}", self.min_arity)
        } else {
            format!("{}-{}", self.min_arity, self.max_arity)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Checker
// ─────────────────────────────────────────────────────────────────────────────

pub struct Checker {
    /// All known function definitions (collected in pass 1).
    fns:         HashMap<String, FnInfo>,
    pub diags:   Vec<Diag>,
}

impl Checker {
    pub fn new() -> Self {
        Checker { fns: HashMap::new(), diags: Vec::new() }
    }

    /// Run both passes and return the diagnostics.
    pub fn check(&mut self, program: &Program) {
        self.collect_fns(program);
        self.check_stmts(program, "");
    }

    // ── Pass 1: collect function names + arities ──────────────────────────

    fn collect_fns(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.collect_fn_stmt(stmt);
        }
    }

    fn collect_fn_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
                if let Some(ref name) = def.name {
                    let info = FnInfo::from_params(&def.params);
                    if self.fns.contains_key(name.as_str()) {
                        self.diags.push(Diag::warning(
                            format!("duplicate function definition `{}`", name),
                            String::new(),
                        ));
                        // Keep the first definition's arity for subsequent checks
                    } else {
                        self.fns.insert(name.clone(), info);
                    }
                    // Recurse into body to pick up nested functions
                    if let FnBody::Block(stmts) = &def.body {
                        self.collect_fns(stmts);
                    }
                }
            }
            Stmt::ImplDecl { methods, .. } | Stmt::TraitDecl { methods, .. } => {
                for m in methods {
                    if let Some(ref name) = m.name {
                        self.fns.insert(name.clone(), FnInfo::from_params(&m.params));
                    }
                }
            }
            // Recurse into control flow to catch locally-defined functions
            Stmt::If { then_body, elif_clauses, else_body, .. } => {
                self.collect_fns(then_body);
                for (_, b) in elif_clauses { self.collect_fns(b); }
                if let Some(b) = else_body { self.collect_fns(b); }
            }
            Stmt::For { body, .. } | Stmt::While { body, .. } => {
                self.collect_fns(body);
            }
            Stmt::Unsafe(b) => { self.collect_fns(b); }
            Stmt::Annotation { inner, .. } => { self.collect_fn_stmt(inner); }
            _ => {}
        }
    }

    // ── Pass 2: check all call sites ─────────────────────────────────────

    fn check_stmts(&mut self, stmts: &[Stmt], ctx: &str) {
        for stmt in stmts {
            self.check_stmt(stmt, ctx);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt, ctx: &str) {
        match stmt {
            Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
                let fn_ctx = def.name.as_deref().unwrap_or("<lambda>").to_string();
                match &def.body {
                    FnBody::Block(stmts) => self.check_stmts(stmts, &fn_ctx),
                    FnBody::Arrow(expr)  => self.check_expr(expr, &fn_ctx),
                    FnBody::Abstract     => {}
                }
            }
            Stmt::Let { value, .. }         => self.check_expr(value, ctx),
            Stmt::Destructure { value, .. } => self.check_expr(value, ctx),
            Stmt::Assign { value, .. }      => self.check_expr(value, ctx),
            Stmt::CompoundAssign { value, .. } => self.check_expr(value, ctx),
            Stmt::Return(Some(e))           => self.check_expr(e, ctx),
            Stmt::Break(Some(e))            => self.check_expr(e, ctx),
            Stmt::Expr(e)                   => self.check_expr(e, ctx),
            Stmt::AwaitStmt(e)              => self.check_expr(e, ctx),
            Stmt::SpawnStmt(e)              => self.check_expr(e, ctx),
            Stmt::Comptime { value, .. }    => self.check_expr(value, ctx),
            Stmt::Annotation { inner, .. }  => self.check_stmt(inner, ctx),
            Stmt::Unsafe(body)              => self.check_stmts(body, ctx),

            Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
                self.check_expr(cond, ctx);
                self.check_stmts(then_body, ctx);
                for (ec, eb) in elif_clauses {
                    self.check_expr(ec, ctx);
                    self.check_stmts(eb, ctx);
                }
                if let Some(body) = else_body { self.check_stmts(body, ctx); }
            }
            Stmt::For { iter, body, .. } => {
                self.check_expr(iter, ctx);
                self.check_stmts(body, ctx);
            }
            Stmt::While { cond, body, .. } => {
                self.check_expr(cond, ctx);
                self.check_stmts(body, ctx);
            }
            Stmt::Match { expr, arms } => {
                self.check_expr(expr, ctx);
                for arm in arms {
                    if let Some(g) = &arm.guard { self.check_expr(g, ctx); }
                    match &arm.body {
                        MatchBody::Expr(e)      => self.check_expr(e, ctx),
                        MatchBody::Block(stmts) => self.check_stmts(stmts, ctx),
                    }
                }
            }
            Stmt::ImplDecl { methods, .. } | Stmt::TraitDecl { methods, .. } => {
                for m in methods {
                    let m_ctx = m.name.as_deref().unwrap_or("<method>").to_string();
                    match &m.body {
                        FnBody::Block(stmts) => self.check_stmts(stmts, &m_ctx),
                        FnBody::Arrow(expr)  => self.check_expr(expr, &m_ctx),
                        FnBody::Abstract     => {}
                    }
                }
            }
            Stmt::Throw(e) => self.check_expr(e, ctx),
            Stmt::TryCatch { body, catches } => {
                self.check_stmts(body, ctx);
                for (_, handler) in catches { self.check_stmts(handler, ctx); }
            }
            // Nothing to check in these
            Stmt::Return(None) | Stmt::Break(None) | Stmt::Continue
            | Stmt::TypeDecl { .. } | Stmt::ExternFn { .. } | Stmt::Import { .. } => {}
        }
    }

    fn check_expr(&mut self, expr: &Expr, ctx: &str) {
        match expr {
            // ── The key check: named function call arity ──────────────────
            Expr::Call { callee, args, kwargs } => {
                if let Expr::Ident(name) = callee.as_ref() {
                    if let Some(info) = self.fns.get(name.as_str()).cloned() {
                        let given = args.len() + kwargs.len();
                        if !info.accepts(given) {
                            let in_ctx = if ctx.is_empty() {
                                String::new()
                            } else {
                                format!("in `{}`", ctx)
                            };
                            self.diags.push(Diag::error(
                                format!("function `{}` takes {} arg{}, got {}",
                                    name,
                                    info.arity_desc(),
                                    if info.min_arity == 1 && info.max_arity == 1 { "" } else { "s" },
                                    given),
                                in_ctx,
                            ));
                        }
                    }
                }
                self.check_expr(callee, ctx);
                for a in args   { self.check_expr(a, ctx); }
                for (_, v) in kwargs { self.check_expr(v, ctx); }
            }

            // ── Recurse into all other expression forms ───────────────────
            Expr::BinOp { lhs, rhs, .. } => {
                self.check_expr(lhs, ctx); self.check_expr(rhs, ctx);
            }
            Expr::UnOp { expr, .. } => { self.check_expr(expr, ctx); }
            Expr::Pipe { lhs, rhs }  => {
                self.check_expr(lhs, ctx); self.check_expr(rhs, ctx);
            }
            Expr::Range { start, end, .. } => {
                self.check_expr(start, ctx); self.check_expr(end, ctx);
            }
            Expr::MethodCall { obj, args, kwargs, .. } => {
                self.check_expr(obj, ctx);
                for a in args   { self.check_expr(a, ctx); }
                for (_, v) in kwargs { self.check_expr(v, ctx); }
            }
            Expr::Index { obj, idx } => {
                self.check_expr(obj, ctx); self.check_expr(idx, ctx);
            }
            Expr::Field { obj, .. } | Expr::OptChain { obj, .. } => {
                self.check_expr(obj, ctx);
            }
            Expr::List(items) | Expr::Set(items) | Expr::Tuple(items) => {
                for i in items { self.check_expr(i, ctx); }
            }
            Expr::Map(pairs) => {
                for (k, v) in pairs {
                    self.check_expr(k, ctx); self.check_expr(v, ctx);
                }
            }
            Expr::Struct { fields, .. } => {
                for f in fields {
                    match f {
                        StructField::Named { value, .. } => self.check_expr(value, ctx),
                        StructField::Spread(e)           => self.check_expr(e, ctx),
                    }
                }
            }
            Expr::Lambda(def) => {
                let lam_ctx = def.name.as_deref().unwrap_or("<lambda>").to_string();
                match &def.body {
                    FnBody::Block(stmts) => self.check_stmts(stmts, &lam_ctx),
                    FnBody::Arrow(e)     => self.check_expr(e, &lam_ctx),
                    FnBody::Abstract     => {}
                }
            }
            Expr::If { cond, then_expr, elif_clauses, else_expr, .. } => {
                self.check_expr(cond, ctx);
                self.check_expr(then_expr, ctx);
                for (ec, eb) in elif_clauses {
                    self.check_expr(ec, ctx); self.check_expr(eb, ctx);
                }
                if let Some(e) = else_expr { self.check_expr(e, ctx); }
            }
            Expr::Match { expr, arms } => {
                self.check_expr(expr, ctx);
                for arm in arms {
                    if let Some(g) = &arm.guard { self.check_expr(g, ctx); }
                    match &arm.body {
                        MatchBody::Expr(e)      => self.check_expr(e, ctx),
                        MatchBody::Block(stmts) => self.check_stmts(stmts, ctx),
                    }
                }
            }
            Expr::Await(e) | Expr::Spawn(e) => { self.check_expr(e, ctx); }
            Expr::Unsafe(stmts) => { self.check_stmts(stmts, ctx); }

            // Terminals — nothing to recurse into
            Expr::Int(_) | Expr::Float(_) | Expr::Str(_) | Expr::Bool(_)
            | Expr::Nil | Expr::Self_ | Expr::Ident(_) | Expr::Placeholder(_) => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Run the static checker and return (errors, warnings).
/// Prints all diagnostics to stderr with `file_path` prefix.
/// Returns `true` if any errors were found.
pub fn check_program(program: &Program, file_path: &str) -> bool {
    let mut checker = Checker::new();
    checker.check(program);

    let mut has_errors = false;
    for d in &checker.diags {
        eprintln!("nuvc: {}: {}", file_path, d);
        if d.level == Level::Error { has_errors = true; }
    }
    if checker.diags.is_empty() {
        eprintln!("nuvc: {}: OK (no issues found)", file_path);
    }
    has_errors
}
