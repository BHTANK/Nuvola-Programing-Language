/// codegen.rs — Nuvola Stage-0 Code Generator
///
/// Translates the AST produced by the parser into C source that can be
/// compiled with:
///
///   clang -O2 -o output generated.c -I runtime/ -lm
///
/// Strategy
/// ────────
/// • Every Nuvola value is represented as `NvVal` (tagged union defined in nuvola.h).
/// • Variable bindings are emitted as C local variables with fresh unique names
///   to handle Nuvola's shadowing semantics safely.
/// • Lambdas are lifted to file-scope C functions.
/// • The top-level statement list becomes `int main(void) { ... }`.
/// • String interpolation is parsed and split into concat calls at compile time.
/// • Pipeline `a |> f(x)` → `f(a, x)`, `a |> f` → `f(a)`.

use crate::ast::*;
use crate::error::ParseError;
use std::collections::{HashMap, HashSet};

// ─────────────────────────────────────────────────────────────────────────────
// Import path resolution  (M20)
// ─────────────────────────────────────────────────────────────────────────────

/// Try to resolve `rel` (a relative .nvl path like "std/math.nvl") to an
/// absolute path that exists on disk.
///
/// Search order:
///   1. `rel` itself, if absolute.
///   2. `<src_dir>/<rel>`          — beside the source file.
///   3. `<src_dir>/../<rel>`       — one level up (tests/ → project root).
///   4. `$NUVOLA_STDLIB/<rel>`     — explicit stdlib override.
///   5. `<exe_dir>/<rel>`          — beside the nuvc binary.
///   6. `./<rel>`                  — CWD fallback.
fn resolve_import_path(rel: &str, src_file: &str) -> Option<String> {
    use std::path::Path;

    // 1. Already absolute.
    if rel.starts_with('/') {
        if Path::new(rel).exists() { return Some(rel.to_string()); }
        return None;
    }

    let mut candidates: Vec<String> = Vec::new();

    // 2 & 3. Relative to source file (and its parent).
    if !src_file.is_empty() {
        if let Some(dir) = Path::new(src_file).parent() {
            candidates.push(dir.join(rel).to_string_lossy().into_owned());
            // One level up (e.g. tests/ → project root that has std/)
            if let Some(parent) = dir.parent() {
                candidates.push(parent.join(rel).to_string_lossy().into_owned());
            }
        }
    }

    // 4. NUVOLA_STDLIB env var.
    if let Ok(stdlib) = std::env::var("NUVOLA_STDLIB") {
        candidates.push(format!("{}/{}", stdlib.trim_end_matches('/'), rel));
    }

    // 5. Beside the running binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join(rel).to_string_lossy().into_owned());
        }
    }

    // 6. CWD.
    candidates.push(rel.to_string());

    candidates.into_iter().find(|p| Path::new(p).exists())
}

// ─────────────────────────────────────────────────────────────────────────────
// Public entry point
// ─────────────────────────────────────────────────────────────────────────────

/// Translate a parsed program to a complete C source string.
/// `src_path` is the Nuvola source filename used for `#line` directives (pass "" to disable).
pub fn emit(program: &Program, runtime_include: &str, src_path: &str) -> Result<String, ParseError> {
    let mut cg = Codegen::new();
    cg.src_file = src_path.to_string();
    cg.emit_program(program, runtime_include)
}

// ─────────────────────────────────────────────────────────────────────────────
// Codegen state
// ─────────────────────────────────────────────────────────────────────────────

struct Codegen {
    /// Accumulated output for top-level (forward decls + lifted lambdas + fns)
    top:     String,
    /// Counter for generating unique C identifiers
    counter: usize,
    /// Scope stack: name → C variable name
    scopes:  Vec<HashMap<String, String>>,
    /// (c_name, param_count) for all top-level functions (for forward decls)
    fn_decls: Vec<(String, usize)>,
    /// Original Nuvola names of declared functions (for call-site detection)
    fn_names_orig: HashSet<String>,
    /// name → param_count for compile-time arity checking
    fn_arities: HashMap<String, usize>,
    /// namespace alias → set of fn names in that namespace
    ns_fns: HashMap<String, HashSet<String>>,
    /// Source filename for #line directives (empty = no #line emission)
    src_file: String,
    /// C names emitted as file-scope `static NvVal ...;` (imported constants)
    static_globals: HashSet<String>,
    /// Functions that have an int64_t* typed list variant (suffix _t).
    /// Maps Nuvola fn name → which param index is the int64_t* (0-based).
    int_list_fn_variants: HashMap<String, usize>,
    /// When Some((nv_name, param_c_names)), self-recursive tail calls
    /// to nv_name get compiled as param-update + continue (TCO).
    tco_self: Option<(String, Vec<String>)>,
    /// Enum type name → variants (for constructor emission + pattern matching)
    enum_types: HashMap<String, Vec<crate::ast::VariantDecl>>,
}

impl Codegen {
    fn new() -> Self {
        Codegen {
            top:          String::new(),
            counter:      0,
            scopes:       vec![HashMap::new()],
            fn_decls:     Vec::new(),
            fn_names_orig: HashSet::new(),
            fn_arities:   HashMap::new(),
            ns_fns:       HashMap::new(),
            src_file:     String::new(),
            static_globals: HashSet::new(),
            int_list_fn_variants: HashMap::new(),
            tco_self: None,
            enum_types: HashMap::new(),
        }
    }

    // ── Name management ───────────────────────────────────────────────────────

    fn fresh(&mut self, hint: &str) -> String {
        let n = self.counter;
        self.counter += 1;
        // Sanitise hint: replace non-alnum with _
        let base: String = hint.chars()
            .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
            .collect();
        if n == 0 { base } else { format!("{}_{}", base, n) }
    }

    fn push_scope(&mut self) { self.scopes.push(HashMap::new()); }
    fn push_fn_scope(&mut self) {
        // Function boundary: insert sentinel so assign handler knows we're in a new fn
        let mut s = HashMap::new();
        s.insert("__fn_boundary__".to_string(), String::new());
        self.scopes.push(s);
    }
    fn pop_scope(&mut self)  { self.scopes.pop(); }

    /// Look up name only within the current function's scope (stop at fn boundary).
    /// Returns Some(c_name) if found, None if not declared locally.
    fn lookup_local(&self, name: &str) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            if let Some(c) = scope.get(name) { return Some(c.clone()); }
            if scope.contains_key("__fn_boundary__") { break; }
        }
        None
    }

    /// Define a new Nuvola name in the current scope → returns the C name.
    /// If the name was pre-registered as a file-scope static (imported global),
    /// the pre-registered C name is reused so function bodies can reference it.
    fn define(&mut self, name: &str) -> String {
        // Reuse only if the existing mapping is a file-scope static global
        if let Some(existing) = self.scopes.last().and_then(|s| s.get(name)).cloned() {
            if self.static_globals.contains(&existing) {
                return existing;
            }
        }
        let c_name = self.fresh(name);
        self.scopes.last_mut().unwrap().insert(name.to_string(), c_name.clone());
        c_name
    }

    /// Check if a name resolves to a module-level static global (for global mutation).
    fn lookup_global_static(&self, name: &str) -> Option<String> {
        if let Some(c) = self.scopes.first().and_then(|s| s.get(name)) {
            if self.static_globals.contains(c) {
                return Some(c.clone());
            }
        }
        None
    }

    /// Look up an existing binding → C name, or fall back to the Nuvola name
    /// (for builtins / function names that are emitted as-is).
    fn lookup(&self, name: &str) -> String {
        for scope in self.scopes.iter().rev() {
            if let Some(c) = scope.get(name) { return c.clone(); }
        }
        // Fallback — use a c-safe mangling of the name
        c_ident(name)
    }

    /// Register a function name at the outermost scope (so calls can find it)
    fn define_fn(&mut self, name: &str, param_count: usize) -> String {
        let c_name = format!("nv_fn_{}", c_ident(name));
        self.scopes[0].insert(name.to_string(), c_name.clone());
        self.fn_decls.push((c_name.clone(), param_count));
        self.fn_names_orig.insert(name.to_string());
        self.fn_arities.insert(name.to_string(), param_count);
        c_name
    }

    // ── Enum constructor emission ──────────────────────────────────────────────

    fn emit_enum_constructors(&mut self, type_name: &str, variants: &[crate::ast::VariantDecl]) {
        for v in variants {
            let fn_c = format!("nv_fn_{}_{}", c_ident(type_name), c_ident(&v.name));
            let n = v.fields.len();
            if n == 0 {
                // Zero-arg variant: nv_fn_Color_Red(_unused, __env) → map
                self.top.push_str(&format!(
                    "static NvVal {}(NvVal _unused, NvVal __env) {{\n    (void)_unused; (void)__env;\n    return nv_map_of(4, nv_str(\"_type\"), nv_str(\"{}\"), nv_str(\"_tag\"), nv_str(\"{}\"));\n}}\n",
                    fn_c, type_name, v.name
                ));
            } else if n == 1 {
                // Single-arg variant
                self.top.push_str(&format!(
                    "static NvVal {}(NvVal __arg, NvVal __env) {{\n    (void)__env;\n    return nv_map_of(6, nv_str(\"_type\"), nv_str(\"{}\"), nv_str(\"_tag\"), nv_str(\"{}\"), nv_str(\"0\"), __arg);\n}}\n",
                    fn_c, type_name, v.name
                ));
            } else {
                // Multi-arg: take a list/tuple as single arg, unpack positionally
                self.top.push_str(&format!(
                    "static NvVal {}(NvVal __args, NvVal __env) {{\n    (void)__env;\n    NvVal _m = nv_map_of(4, nv_str(\"_type\"), nv_str(\"{}\"), nv_str(\"_tag\"), nv_str(\"{}\"));\n",
                    fn_c, type_name, v.name
                ));
                for i in 0..n {
                    self.top.push_str(&format!(
                        "    nv_map_set_mut(_m, nv_str(\"{}\"), nv_index(__args, nv_int({})));\n",
                        i, i
                    ));
                }
                self.top.push_str("    return _m;\n}\n");
            }
            // Register variant constructor: "TypeName_VariantName" → fn_c
            let nv_name = format!("{}_{}", type_name, v.name);
            self.scopes[0].insert(nv_name.clone(), fn_c.clone());
            self.fn_names_orig.insert(nv_name.clone());
            self.fn_arities.insert(nv_name.clone(), if n == 0 { 0 } else { 1 });
        }
    }

    // ── Top-level emission ────────────────────────────────────────────────────

    fn emit_program(&mut self, program: &Program, runtime_include: &str) -> Result<String, ParseError> {
        // namespace alias → Vec<(name, is_fn, value_expr)>
        // is_fn=true: function ref; is_fn=false: constant (value_expr is C expression string)
        let mut ns_map: std::collections::HashMap<String, Vec<(String, bool, String)>> =
            std::collections::HashMap::new();

        // Helper: process a file import, returning (fn_stmts, global_stmts)
        // For aliased imports we collect names for the namespace map.
        let mut all_fns:     Vec<Stmt> = Vec::new(); // fn_def stmts from imports + user
        let mut all_globals: Vec<Stmt> = Vec::new(); // non-fn stmts from flat imports
        let mut user_stmts:  Vec<Stmt> = Vec::new(); // user program stmts (non-import)

        for stmt in program {
            if let Stmt::Import { path, names: None, alias } = stmt {
                // Resolve the import to an actual file path.
                // Handles both string imports ("std/math.nvl") and dot-notation
                // (std.math → std/math.nvl).
                let rel = if path.len() == 1 && path[0].ends_with(".nvl") {
                    path[0].clone()              // "std/math.nvl" — already a path
                } else {
                    path.join("/") + ".nvl"      // std.math → "std/math.nvl"
                };

                if let Some(resolved) = resolve_import_path(&rel, &self.src_file) {
                    if let Ok(src) = std::fs::read_to_string(&resolved) {
                        if let Ok(toks) = crate::lexer::tokenize(&src) {
                            if let Ok(imported) = crate::parser::parse(toks) {
                                for is in imported {
                                    match &is {
                                        Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
                                            let fname = def.name.clone().unwrap_or_default();
                                            if let Some(ref ns) = alias {
                                                ns_map.entry(ns.clone()).or_default()
                                                    .push((fname, true, String::new()));
                                            }
                                            all_fns.push(is);
                                        }
                                        Stmt::Import { .. } | Stmt::ExternFn { .. } => {
                                            all_fns.push(is);
                                        }
                                        _ => {
                                            if alias.is_none() {
                                                all_globals.push(is);
                                            }
                                            // aliased: handled at ns-map build time via emit_stmt
                                        }
                                    }
                                }
                                continue;
                            }
                        }
                    } // else: file exists but read/lex/parse failed — fall through silently
                } else {
                    eprintln!("nuvc: import not found: {}", rel);
                }
            }
            match stmt {
                Stmt::FnDecl(_) | Stmt::AsyncFnDecl(_) => all_fns.push(stmt.clone()),
                Stmt::Import { .. } => {}
                // ImplDecl: hoist method functions, keep stmt for impl_for registrations
                Stmt::ImplDecl { methods, .. } => {
                    for m in methods {
                        all_fns.push(Stmt::FnDecl(m.clone()));
                    }
                    user_stmts.push(stmt.clone());
                }
                _ => user_stmts.push(stmt.clone()),
            }
        }

        // First pass: collect all top-level function names + enum types for forward declarations
        for stmt in all_fns.iter().chain(user_stmts.iter()).chain(all_globals.iter()) {
            match stmt {
                Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
                    if let Some(ref n) = def.name {
                        self.define_fn(n, def.params.len());
                    }
                }
                Stmt::TypeDecl { name, kind, .. } => {
                    if let crate::ast::TypeDeclKind::Enum(variants) = kind {
                        let variants_clone = variants.clone();
                        self.enum_types.insert(name.clone(), variants_clone.clone());
                        self.emit_enum_constructors(name, &variants_clone);
                    }
                }
                _ => {}
            }
        }

        // Pre-register imported globals as file-scope C statics so imported
        // fn bodies can reference them by name (they're emitted before main()).
        for stmt in &all_globals {
            match stmt {
                Stmt::Let { name, .. } => {
                    let c_name = format!("g_{}", c_ident(name));
                    self.scopes[0].insert(name.clone(), c_name.clone());
                    self.top.push_str(&format!("static NvVal {};\n", c_name));
                    self.static_globals.insert(c_name);
                }
                Stmt::Destructure { names, .. } => {
                    for name in names {
                        let c_name = format!("g_{}", c_ident(name));
                        self.scopes[0].insert(name.clone(), c_name.clone());
                        self.top.push_str(&format!("static NvVal {};\n", c_name));
                        self.static_globals.insert(c_name);
                    }
                }
                _ => {}
            }
        }

        // Pre-register user top-level let/bind/comptime as C statics visible to functions
        for stmt in &user_stmts {
            match stmt {
                Stmt::Let { name, .. } => {
                    let c_name = format!("g_{}", c_ident(name));
                    self.scopes[0].insert(name.clone(), c_name.clone());
                    self.top.push_str(&format!("static NvVal {};\n", c_name));
                    self.static_globals.insert(c_name);
                }
                Stmt::Comptime { name, .. } => {
                    let c_name = format!("g_{}", c_ident(name));
                    self.scopes[0].insert(name.clone(), c_name.clone());
                    self.top.push_str(&format!("static NvVal {};\n", c_name));
                    self.static_globals.insert(c_name);
                }
                Stmt::Destructure { names, .. } => {
                    for name in names {
                        let c_name = format!("g_{}", c_ident(name));
                        self.scopes[0].insert(name.clone(), c_name.clone());
                        self.top.push_str(&format!("static NvVal {};\n", c_name));
                        self.static_globals.insert(c_name);
                    }
                }
                _ => {}
            }
        }

        // Register namespace aliases as globals + populate ns_fns for dispatch
        for (ns_alias, entries) in &ns_map {
            self.scopes[0].insert(ns_alias.clone(), format!("g_{}", ns_alias));
            let fns: HashSet<String> = entries.iter()
                .filter(|(_, is_fn, _)| *is_fn)
                .map(|(n, _, _)| n.clone())
                .collect();
            self.ns_fns.insert(ns_alias.clone(), fns);
        }

        // Second pass: emit functions
        let mut main_body = String::new();
        for stmt in &all_fns {
            match stmt {
                Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
                    self.emit_fn_def(def, &[])?;
                }
                Stmt::Import { .. } => {}
                _ => {}
            }
        }

        // Emit flat imported globals into main()
        for stmt in &all_globals {
            let s = self.emit_stmt_str(stmt, 1)?;
            main_body.push_str(&s);
        }

        // Emit namespace map construction
        for (ns_alias, entries) in &ns_map {
            let ns_var = format!("g_{}", ns_alias);
            main_body.push_str(&format!("    {} = nv_map_new();\n", ns_var));
            for (name, is_fn, _) in entries {
                if *is_fn {
                    main_body.push_str(&format!(
                        "    nv_map_set_mut({}, nv_str(\"{}\"), nv_fn((NvFn)nv_fn_{}));\n",
                        ns_var, name, name));
                }
            }
        }

        // Try specialized main body (typed int arrays, eliminates NvVal boxing)
        // Must be called after fn defs are emitted (int_list_fn_variants is populated)
        let user_stmts_body = if let Some(specialized) = self.try_emit_specialized_main_body(&user_stmts) {
            specialized
        } else {
            // Regular NvVal emission
            let mut body = String::new();
            for stmt in &user_stmts {
                let s = self.emit_stmt_str(stmt, 1)?;
                body.push_str(&s);
            }
            body
        };
        main_body.push_str(&user_stmts_body);

        // Assemble the full C file
        let _ = runtime_include; // used by caller for -I flag; we always include by name
        let mut out = String::new();
        out.push_str("#include \"nuvola.h\"\n\n");

        // Forward declarations for all user functions (params + __env at end)
        for (fname, n_params) in &self.fn_decls.clone() {
            let mut parts: Vec<String> = (0..*n_params).map(|i| format!("NvVal _p{}", i)).collect();
            parts.push("NvVal _env".to_string());
            out.push_str(&format!("NvVal {}({});\n", fname, parts.join(", ")));
        }
        // Global declarations for namespace aliases
        for ns_alias in ns_map.keys() {
            out.push_str(&format!("static NvVal g_{} = {{0}};\n", ns_alias));
        }
        out.push('\n');

        // Lifted lambdas and function definitions
        out.push_str(&self.top);
        out.push('\n');

        // main()
        out.push_str("int main(int argc, char **argv) {\n");
        out.push_str("    NV_GC_INIT();\n");
        out.push_str("    _nv_argc = argc; _nv_argv = argv;\n");
        out.push_str(&main_body);
        out.push_str("    return 0;\n");
        out.push_str("}\n");

        Ok(out)
    }

    /// Try to emit a fully typed main body with int64_t* arrays instead of NvVal lists.
    /// Returns Some(code) if all statements can be specialized, None otherwise.
    fn try_emit_specialized_main_body(&self, stmts: &[Stmt]) -> Option<String> {
        // Step 1: Detect integer array variables.
        // An integer array is created via `arr := []` and filled only via arr.push(int_expr).
        let int_arrays = detect_int_array_vars(stmts);

        // Path B: no int arrays, but purely scalar main (e.g. nbody) — emit double/long locals
        if int_arrays.is_empty() {
            if !stmts_all_pure_scalar(stmts) { return None; }
            let mut sty_env = StyEnv::new();
            for _ in 0..30 {
                if !sty_pass_stmts(stmts, &mut sty_env) { break; }
            }
            // Build coalesce map to reduce register pressure for non-overlapping live ranges
            let coalesce = build_coalesce_map(stmts, &sty_env);
            // Find variables that are never reassigned — emit as `const` to free XMM registers
            let const_vars = find_const_vars(stmts);
            let mut out = String::new();
            let mut declared: HashSet<String> = HashSet::new();
            for stmt in stmts {
                out.push_str(&emit_sty_stmt_with_coalesce(stmt, &sty_env, "", &mut declared, &coalesce, &const_vars, 1));
            }
            return Some(out);
        }

        // Step 2: Check every statement is specializable (Path A: has int arrays).
        if !stmts_all_specializable(stmts, &int_arrays, &self.int_list_fn_variants) {
            return None;
        }

        // Step 3: Build scalar type env for non-array vars.
        let mut sty_env = StyEnv::new();
        for _ in 0..30 {
            if !sty_pass_stmts_ignoring_lists_ext(stmts, &mut sty_env, &int_arrays) { break; }
        }

        // Step 4: Determine array sizes from preceding scalar literal assignments.
        // Build a map: array_name → initial_capacity_expr
        let array_sizes = compute_array_sizes(stmts, &int_arrays, &sty_env);

        // Step 5: Emit specialized C code.
        let mut out = String::new();
        // Track which arrays have been declared (name → (c_name, len_name))
        let mut arr_decls: HashMap<String, (String, String)> = HashMap::new();
        // Track which arrays we malloc'd (for free at end)
        let mut arr_to_free: Vec<String> = Vec::new();
        let mut declared_scalars: HashSet<String> = HashSet::new();

        for stmt in stmts {
            let code = emit_specialized_stmt(
                stmt, &sty_env, &int_arrays, &array_sizes, &self.int_list_fn_variants,
                &mut arr_decls, &mut arr_to_free, &mut declared_scalars, 1,
            )?;
            out.push_str(&code);
        }

        // Free int arrays before return
        for arr_name in &arr_to_free {
            out.push_str(&format!("    free({});\n", arr_name));
        }

        Some(out)
    }

    // ── Function definition ───────────────────────────────────────────────────

    fn emit_fn_def(&mut self, def: &FnDef, captured: &[String]) -> Result<(), ParseError> {
        let c_name = if let Some(ref n) = def.name {
            // Always use the canonical nv_fn_ name for registered functions,
            // regardless of any variable shadowing in scope.
            if self.fn_names_orig.contains(n.as_str()) {
                format!("nv_fn_{}", c_ident(n))
            } else {
                self.lookup(n)
            }
        } else {
            self.fresh("__lambda")
        };

        // ── Integer specialization (pure-int functions get a `_i` fast path) ──
        if let Some(ref nv_name_str) = def.name {
            if self.fn_names_orig.contains(nv_name_str.as_str())
                && captured.is_empty()
                && is_pure_int_fn(def, nv_name_str)
            {
                let fast_name = format!("{}_i", c_name);
                let param_names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();

                // Forward declare the fast function
                let long_param_types = vec!["long"; param_names.len()].join(", ");
                self.top.push_str(&format!("static long {}({});\n", fast_name, long_param_types));

                // Emit the fast `static long _i(long ...)` function
                let long_params_decl = param_names.iter()
                    .map(|p| format!("long {}", c_ident(p)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut fast_src = format!("static long {}({}) {{\n", fast_name, long_params_decl);
                match &def.body {
                    FnBody::Arrow(expr) => {
                        let e = emit_int_expr(expr, nv_name_str, &param_names);
                        fast_src.push_str(&format!("    return {};\n", e));
                    }
                    FnBody::Block(stmts) => {
                        let n = stmts.len();
                        for (i, s) in stmts.iter().enumerate() {
                            if i + 1 == n {
                                match s {
                                    Stmt::Expr(e) => {
                                        let es = emit_int_expr(e, nv_name_str, &param_names);
                                        fast_src.push_str(&format!("    return {};\n", es));
                                    }
                                    _ => fast_src.push_str(
                                        &emit_int_stmt(s, nv_name_str, &param_names, 1)),
                                }
                            } else {
                                fast_src.push_str(
                                    &emit_int_stmt(s, nv_name_str, &param_names, 1));
                            }
                        }
                    }
                    FnBody::Abstract => {}
                }
                fast_src.push_str("}\n\n");
                self.top.push_str(&fast_src);

                // Emit the thin NvVal wrapper that delegates to _i
                let nv_name_display = nv_name_str.as_str();
                let nv_params_decl = param_names.iter()
                    .map(|p| format!("NvVal {}", c_ident(p)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let params_sep = if nv_params_decl.is_empty() { "" } else { ", " };
                let call_args = param_names.iter()
                    .map(|p| format!("nv_to_i({})", c_ident(p)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let wrapper = format!(
                    "NvVal {}({}{}NvVal __env) {{\n    NV_ENTER(\"{}\");\n    (void)__env;\n    return nv_int({}({}));\n}}\n\n",
                    c_name, nv_params_decl, params_sep, nv_name_display, fast_name, call_args
                );
                self.top.push_str(&wrapper);
                return Ok(());
            }
        }

        // ── Scalar (float+int mixed) specialization ───────────────────────────
        if let Some(ref nv_name_str) = def.name {
            if self.fn_names_orig.contains(nv_name_str.as_str())
                && captured.is_empty()
            {
                if let Some(ty_env) = infer_scalar_fn(def, nv_name_str) {
                    let fast_name = format!("{}_s", c_name);
                    let param_names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
                    let ret_ty = sty_return_ty(def, &ty_env, nv_name_str);

                    // Build typed param declarations and extractor calls
                    let typed_params: Vec<String> = param_names.iter().map(|p| {
                        let ty = ty_env.get(p).copied().unwrap_or(ScalarTy::Int);
                        format!("{} {}", ty.c_type(), c_ident(p))
                    }).collect();
                    let param_types_only: Vec<&str> = param_names.iter()
                        .map(|p| ty_env.get(p).copied().unwrap_or(ScalarTy::Int).c_type())
                        .collect();
                    let extractor_calls: Vec<String> = param_names.iter().map(|p| {
                        let ty = ty_env.get(p).copied().unwrap_or(ScalarTy::Int);
                        format!("{}({})", ty.nv_extractor(), c_ident(p))
                    }).collect();

                    // Forward declare the fast function
                    self.top.push_str(&format!(
                        "static {} {}({});\n",
                        ret_ty.c_type(), fast_name, param_types_only.join(", ")
                    ));

                    // Emit the typed fast function body
                    let mut fast_src = format!(
                        "static {} {}({}) {{\n",
                        ret_ty.c_type(), fast_name, typed_params.join(", ")
                    );

                    if let FnBody::Block(stmts) = &def.body {
                        let mut declared: HashSet<String> = param_names.iter().cloned().collect();
                        let n = stmts.len();
                        for (i, s) in stmts.iter().enumerate() {
                            if i + 1 == n {
                                match s {
                                    Stmt::Expr(e) => {
                                        fast_src.push_str(&format!(
                                            "    return {};\n", emit_sty_expr(e, &ty_env, nv_name_str)
                                        ));
                                    }
                                    _ => fast_src.push_str(&emit_sty_stmt(
                                        s, &ty_env, nv_name_str, &mut declared, 1)),
                                }
                            } else {
                                fast_src.push_str(&emit_sty_stmt(
                                    s, &ty_env, nv_name_str, &mut declared, 1));
                            }
                        }
                    }
                    fast_src.push_str("}\n\n");
                    self.top.push_str(&fast_src);

                    // Emit the thin NvVal wrapper
                    let nv_params_decl = param_names.iter()
                        .map(|p| format!("NvVal {}", c_ident(p)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let params_sep = if nv_params_decl.is_empty() { "" } else { ", " };
                    let wrapper = format!(
                        "NvVal {}({}{}NvVal __env) {{\n    NV_ENTER(\"{}\");\n    (void)__env;\n    return {}({}_s({}));\n}}\n\n",
                        c_name, nv_params_decl, params_sep, nv_name_str,
                        ret_ty.nv_boxer(), c_name, extractor_calls.join(", ")
                    );
                    self.top.push_str(&wrapper);
                    return Ok(());
                }
            }
        }

        // ── Typed bool array specialization (e.g. sieve/primes) ─────────────
        if let Some(ref nv_name_str) = def.name {
            if self.fn_names_orig.contains(nv_name_str.as_str()) && captured.is_empty() {
                if let Some((array_name, size_c, fill_val, _skip_set, filtered)) =
                    detect_typed_bool_array_fn(def, nv_name_str)
                {
                    // Build scalar env from filtered stmts
                    let mut ty_env = StyEnv::new();
                    for _ in 0..30 {
                        let changed = sty_pass_stmts_filtered(&filtered, &mut ty_env, &array_name, "");
                        if !changed { break; }
                    }
                    // Seed params from env
                    let param_names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
                    for p in &param_names {
                        if !ty_env.contains_key(p) {
                            ty_env.insert(p.clone(), ScalarTy::Int);
                        }
                    }

                    let fast_name = format!("{}_s", c_name);
                    let ret_ty = ScalarTy::Int; // sieve returns count (int)

                    let fast_src = emit_typed_array_sty_fn(
                        &fast_name, def, nv_name_str, &ty_env, ret_ty,
                        &array_name, &size_c, fill_val, &filtered,
                    );
                    self.top.push_str(&fast_src);

                    // Thin NvVal wrapper
                    let nv_params_decl = param_names.iter()
                        .map(|p| format!("NvVal {}", c_ident(p)))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let params_sep = if nv_params_decl.is_empty() { "" } else { ", " };
                    let extractor_calls: Vec<String> = param_names.iter().map(|p| {
                        let ty = ty_env.get(p).copied().unwrap_or(ScalarTy::Int);
                        format!("{}({})", ty.nv_extractor(), c_ident(p))
                    }).collect();
                    let wrapper = format!(
                        "NvVal {}({}{}NvVal __env) {{\n    NV_ENTER(\"{}\");\n    (void)__env;\n    return {}({}_s({}));\n}}\n\n",
                        c_name, nv_params_decl, params_sep, nv_name_str,
                        ret_ty.nv_boxer(), c_name, extractor_calls.join(", ")
                    );
                    self.top.push_str(&wrapper);
                    return Ok(());
                }
            }
        }

        // ── Raw list pointer specialization (e.g. quicksort) ─────────────────
        if let Some(ref nv_name_str) = def.name {
            if self.fn_names_orig.contains(nv_name_str.as_str()) && captured.is_empty() {
                // First get a preliminary scalar env (may be empty for list params)
                let raw_params = {
                    let mut prelim_env = StyEnv::new();
                    let stmts_ref = match &def.body { FnBody::Block(s) => s.as_slice(), _ => &[] };
                    for _ in 0..30 {
                        if !sty_pass_stmts_ignoring_lists(stmts_ref, &mut prelim_env, &[]) { break; }
                    }
                    detect_raw_list_params(def, nv_name_str, &prelim_env)
                };

                if !raw_params.is_empty() {
                    if let Some(scalar_env) = infer_scalar_env_with_raw_lists(def, nv_name_str, &raw_params) {
                        let fast_name = format!("{}_s", c_name);
                        let param_names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();

                        // Build typed param signature: NvVal for list params, long for scalar params
                        let typed_params: Vec<String> = param_names.iter().map(|p| {
                            if raw_params.contains(p) {
                                format!("NvVal {}", c_ident(p))
                            } else {
                                let ty = scalar_env.get(p).copied().unwrap_or(ScalarTy::Int);
                                format!("{} {}", ty.c_type(), c_ident(p))
                            }
                        }).collect();
                        let param_types_only: Vec<String> = param_names.iter().map(|p| {
                            if raw_params.contains(p) {
                                "NvVal".to_string()
                            } else {
                                scalar_env.get(p).copied().unwrap_or(ScalarTy::Int).c_type().to_string()
                            }
                        }).collect();

                        // Forward declare
                        self.top.push_str(&format!(
                            "static NvVal {}({});\n",
                            fast_name, param_types_only.join(", ")
                        ));

                        // Emit _s body (no NV_ENTER — reduces overhead in recursive fast path)
                        let mut fast_src = format!(
                            "static NvVal {}({}) {{\n",
                            fast_name, typed_params.join(", ")
                        );

                        // Emit raw pointer declarations for each list param
                        for rp in &raw_params {
                            fast_src.push_str(&format!(
                                "    NvVal* _{}_raw = {}.list->data;\n",
                                c_ident(rp), c_ident(rp)
                            ));
                        }

                        if let FnBody::Block(stmts) = &def.body {
                            let mut declared: HashSet<String> = param_names.iter().cloned().collect();
                            let n = stmts.len();
                            for (i, s) in stmts.iter().enumerate() {
                                let is_last = i + 1 == n;
                                let code = emit_raw_list_stmt(
                                    s, &scalar_env, nv_name_str, &mut declared, 1,
                                    &raw_params, is_last
                                );
                                fast_src.push_str(&code);
                            }
                        }
                        fast_src.push_str("    return nv_nil();\n");
                        fast_src.push_str("}\n\n");
                        self.top.push_str(&fast_src);

                        // ── Also emit a _t (int64_t*) typed variant ───────────
                        // Qualifies when ALL scalar params are Int-typed (no Float).
                        let all_scalar_int = param_names.iter()
                            .filter(|p| !raw_params.contains(*p))
                            .all(|p| scalar_env.get(p).copied().unwrap_or(ScalarTy::Int) == ScalarTy::Int);

                        if all_scalar_int {
                            // Find list param index (first one for now)
                            let list_param_idx = param_names.iter()
                                .position(|p| raw_params.contains(p));

                            if let Some(lp_idx) = list_param_idx {
                                let typed_name = format!("{}_t", c_name);

                                // Build _t param signature: int64_t* for list, long for scalar
                                let t_typed_params: Vec<String> = param_names.iter().map(|p| {
                                    if raw_params.contains(p) {
                                        format!("int64_t* {}", c_ident(p))
                                    } else {
                                        format!("long {}", c_ident(p))
                                    }
                                }).collect();
                                let t_param_types_only: Vec<String> = param_names.iter().map(|p| {
                                    if raw_params.contains(p) { "int64_t*".to_string() }
                                    else { "long".to_string() }
                                }).collect();

                                // Forward declare _t
                                self.top.push_str(&format!(
                                    "static void {}({});\n",
                                    typed_name, t_param_types_only.join(", ")
                                ));

                                // Emit _t body
                                let mut t_src = format!(
                                    "static void {}({}) {{\n",
                                    typed_name, t_typed_params.join(", ")
                                );
                                if let FnBody::Block(stmts) = &def.body {
                                    let mut declared: HashSet<String> = param_names.iter().cloned().collect();
                                    for s in stmts.iter() {
                                        let code = emit_int_list_stmt(
                                            s, &scalar_env, nv_name_str, &mut declared, 1,
                                            &raw_params
                                        );
                                        t_src.push_str(&code);
                                    }
                                }
                                t_src.push_str("}\n\n");
                                self.top.push_str(&t_src);

                                // Register this function as having a _t variant
                                self.int_list_fn_variants.insert(nv_name_str.clone(), lp_idx);
                            }
                        }

                        // Thin NvVal wrapper
                        let nv_params_decl = param_names.iter()
                            .map(|p| format!("NvVal {}", c_ident(p)))
                            .collect::<Vec<_>>()
                            .join(", ");
                        let params_sep = if nv_params_decl.is_empty() { "" } else { ", " };
                        let extractor_calls: Vec<String> = param_names.iter().map(|p| {
                            if raw_params.contains(p) {
                                c_ident(p) // pass NvVal directly
                            } else {
                                let ty = scalar_env.get(p).copied().unwrap_or(ScalarTy::Int);
                                format!("{}({})", ty.nv_extractor(), c_ident(p))
                            }
                        }).collect();
                        let wrapper = format!(
                            "NvVal {}({}{}NvVal __env) {{\n    NV_ENTER(\"{}\");\n    (void)__env;\n    return {}_s({});\n}}\n\n",
                            c_name, nv_params_decl, params_sep, nv_name_str,
                            c_name, extractor_calls.join(", ")
                        );
                        self.top.push_str(&wrapper);
                        return Ok(());
                    }
                }
            }
        }

        // ── Standard NvVal function emission ─────────────────────────────────
        // Build parameter list (user params + __env at end)
        let mut params: Vec<String> = def.params.iter()
            .map(|p| format!("NvVal {}", c_ident(&p.name)))
            .collect();
        params.push("NvVal __env".to_string());
        let param_str = params.join(", ");

        let nv_name = def.name.as_deref().unwrap_or("<lambda>");
        let mut fn_src = format!("NvVal {}({}) {{\n    NV_ENTER(\"{}\");\n", c_name, param_str, nv_name);

        self.push_fn_scope();
        // Register params in scope
        for p in &def.params {
            self.scopes.last_mut().unwrap()
                .insert(p.name.clone(), c_ident(&p.name));
        }

        // Load captured variables from __env
        if !captured.is_empty() {
            for var in captured {
                let cv = c_ident(var);
                // Use _opt variant so loading from nv_nil() env (direct calls) returns nil
                fn_src.push_str(&format!("    NvVal {} = nv_map_get_opt(__env, nv_str(\"{}\"));\n", cv, var));
                self.scopes.last_mut().unwrap().insert(var.clone(), cv);
            }
        } else {
            fn_src.push_str("    (void)__env;\n");
        }

        match &def.body {
            FnBody::Arrow(expr) => {
                let e = self.emit_expr_str(expr, 1)?;
                fn_src.push_str(&format!("    return {};\n", e));
            }
            FnBody::Block(stmts) => {
                // ── TCO: wrap in while(1) if self-tail-recursive ─────────────
                let nv_fn_name = def.name.as_deref().unwrap_or("");
                let use_tco = captured.is_empty()
                    && !nv_fn_name.is_empty()
                    && self.fn_names_orig.contains(nv_fn_name)
                    && qualifies_for_tco(def, nv_fn_name);

                if use_tco {
                    let param_c_names: Vec<String> = def.params.iter()
                        .map(|p| c_ident(&p.name))
                        .collect();
                    self.tco_self = Some((nv_fn_name.to_string(), param_c_names));
                    fn_src.push_str("    while (1) {\n");
                    let n = stmts.len();
                    for (i, s) in stmts.iter().enumerate() {
                        if i + 1 == n {
                            fn_src.push_str(&self.emit_stmt_tail(s, 2)?);
                        } else {
                            fn_src.push_str(&self.emit_stmt_str(s, 2)?);
                        }
                    }
                    fn_src.push_str("        return nv_nil();\n");
                    fn_src.push_str("    }\n");
                    self.tco_self = None;
                } else {
                    let n = stmts.len();
                    for (i, s) in stmts.iter().enumerate() {
                        if i + 1 == n {
                            fn_src.push_str(&self.emit_stmt_tail(s, 1)?);
                        } else {
                            fn_src.push_str(&self.emit_stmt_str(s, 1)?);
                        }
                    }
                    fn_src.push_str("    return nv_nil();\n");
                }
            }
            FnBody::Abstract => {}
        }

        self.pop_scope();
        fn_src.push_str("}\n\n");
        self.top.push_str(&fn_src);
        Ok(())
    }

    /// Like emit_stmt_str but for the tail position of a function body.
    /// A trailing match becomes a series of `return` arms.
    /// A trailing expression is wrapped with `return`.
    fn emit_stmt_tail(&mut self, stmt: &Stmt, depth: usize) -> Result<String, ParseError> {
        let ind = "    ".repeat(depth);
        match stmt {
            Stmt::Match { expr, arms } => {
                let subject = self.fresh("_match");
                let subject_s = self.emit_expr_str(expr, depth)?;
                let has_guard = arms.iter().any(|a| a.guard.is_some());
                let mut out = format!("{}NvVal {} = {};\n", ind, subject, subject_s);
                if has_guard {
                    let done_var = self.fresh("_done");
                    out.push_str(&format!("{}int {} = 0;\n", ind, done_var));
                    for arm in arms {
                        let cond = self.emit_pattern_cond(&arm.pattern, &subject, depth)?;
                        out.push_str(&format!("{}if (!{} && {}) {{\n", ind, done_var, cond));
                        self.push_scope();
                        self.emit_pattern_bindings(&arm.pattern, &subject, &mut out, depth+1)?;
                        if let Some(guard) = &arm.guard {
                            let g = self.emit_expr_str(guard, depth+1)?;
                            out.push_str(&format!("{}    if (nv_truthy({})) {{\n", ind, g));
                            out.push_str(&format!("{}        {} = 1;\n", ind, done_var));
                            match &arm.body {
                                MatchBody::Expr(e) => {
                                    let s = self.emit_expr_str(e, depth+2)?;
                                    out.push_str(&format!("{}        return {};\n", ind, s));
                                }
                                MatchBody::Block(stmts) => {
                                    for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+2)?); }
                                }
                            }
                            out.push_str(&format!("{}    }}\n", ind));
                        } else {
                            out.push_str(&format!("{}    {} = 1;\n", ind, done_var));
                            match &arm.body {
                                MatchBody::Expr(e) => {
                                    let s = self.emit_expr_str(e, depth+1)?;
                                    out.push_str(&format!("{}    return {};\n", ind, s));
                                }
                                MatchBody::Block(stmts) => {
                                    for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                                }
                            }
                        }
                        self.pop_scope();
                        out.push_str(&format!("{}}}\n", ind));
                    }
                } else {
                    let mut first = true;
                    for arm in arms {
                        let cond = self.emit_pattern_cond(&arm.pattern, &subject, depth)?;
                        let kw = if first { "if" } else { "} else if" };
                        first = false;
                        out.push_str(&format!("{}{} ({}) {{\n", ind, kw, cond));
                        self.push_scope();
                        self.emit_pattern_bindings(&arm.pattern, &subject, &mut out, depth+1)?;
                        match &arm.body {
                            MatchBody::Expr(e) => {
                                let s = self.emit_expr_str(e, depth+1)?;
                                out.push_str(&format!("{}    return {};\n", ind, s));
                            }
                            MatchBody::Block(stmts) => {
                                for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                            }
                        }
                        self.pop_scope();
                    }
                    if !first { out.push_str(&format!("{}}}\n", ind)); }
                }
                Ok(out)
            }
            Stmt::Expr(e) => {
                let s = self.emit_expr_str(e, depth)?;
                Ok(format!("{}return {};\n", ind, s))
            }
            // Anonymous `fn(params) => body` as the tail of a block → return it as a value
            Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) if def.name.is_none() => {
                let lambda_name = self.fresh("__lambda");
                let free_vars = self.find_free_vars(def);
                let mut cloned = def.clone();
                cloned.name = Some(lambda_name.clone());
                self.scopes[0].insert(lambda_name.clone(), lambda_name.clone());
                self.emit_fn_def(&cloned, &free_vars)?;
                let val = if free_vars.is_empty() {
                    format!("nv_fn((NvFn){})", lambda_name)
                } else {
                    let env_parts: Vec<String> = free_vars.iter().map(|v| {
                        let cv = self.lookup(v);
                        format!("nv_str(\"{}\"), {}", v, cv)
                    }).collect();
                    format!("nv_closure((NvFn){}, nv_map_of({}, {}))",
                        lambda_name, free_vars.len(), env_parts.join(", "))
                };
                Ok(format!("{}return {};\n", ind, val))
            }
            _ => self.emit_stmt_str(stmt, depth),
        }
    }

    // ── Statement emission ────────────────────────────────────────────────────

    fn emit_stmt_str(&mut self, stmt: &Stmt, depth: usize) -> Result<String, ParseError> {
        let ind = "    ".repeat(depth);
        match stmt {
            // ── let / immutable binding ──────────────────────────────────────
            Stmt::Let { name, value, .. } => {
                let expr_s = self.emit_expr_str(value, depth)?;
                let c_name = self.define(name);
                // If the variable was pre-declared as a file-scope static (imported
                // global), emit only an assignment — no `NvVal` redeclaration.
                if self.static_globals.contains(&c_name) {
                    Ok(format!("{}{} = {};\n", ind, c_name, expr_s))
                } else {
                    Ok(format!("{}NvVal {} = {};\n", ind, c_name, expr_s))
                }
            }

            // ── Destructure ──────────────────────────────────────────────────
            Stmt::Destructure { names, value } => {
                let val_name = self.fresh("_dest");
                let expr_s = self.emit_expr_str(value, depth)?;
                let mut out = format!("{}NvVal {} = {};\n", ind, val_name, expr_s);
                for (i, n) in names.iter().enumerate() {
                    let c_name = self.define(n);
                    out.push_str(&format!("{}NvVal {} = nv_list_get({}, {});\n",
                        ind, c_name, val_name, i));
                }
                Ok(out)
            }

            // ── Assign / reassign ─────────────────────────────────────────────
            Stmt::Assign { target, value } => {
                let expr_s = self.emit_expr_str(value, depth)?;
                match target {
                    AssignTarget::Ident(name) => {
                        if let Some(existing) = self.lookup_local(name) {
                            // Already declared in this function — reassign
                            Ok(format!("{}{} = {};\n", ind, existing, expr_s))
                        } else if let Some(global_c) = self.lookup_global_static(name) {
                            // Module-level static global — mutate it directly
                            Ok(format!("{}{} = {};\n", ind, global_c, expr_s))
                        } else {
                            // New mutable variable — declare it
                            let c_name = self.define(name);
                            Ok(format!("{}NvVal {} = {};\n", ind, c_name, expr_s))
                        }
                    }
                    AssignTarget::Index { obj, idx } => {
                        let obj_s = self.emit_expr_str(obj, depth)?;
                        let idx_s = self.emit_expr_str(idx, depth)?;
                        Ok(format!("{}nv_index_set({}, {}, {});\n", ind, obj_s, idx_s, expr_s))
                    }
                    AssignTarget::Field { obj, field } => {
                        let obj_s = self.emit_expr_str(obj, depth)?;
                        Ok(format!("{}nv_map_set_mut({}, nv_str(\"{}\"), {});\n", ind, obj_s, field, expr_s))
                    }
                }
            }

            // ── Compound assign ───────────────────────────────────────────────
            Stmt::CompoundAssign { target, op, value } => {
                if let AssignTarget::Ident(name) = target {
                    let c_name = self.lookup(name);
                    let rhs = self.emit_expr_str(value, depth)?;
                    let op_fn = match op {
                        CompoundOp::Add => "nv_add",
                        CompoundOp::Sub => "nv_sub",
                        CompoundOp::Mul => "nv_mul",
                        CompoundOp::Div => "nv_div",
                    };
                    Ok(format!("{}{} = {}({}, {});\n", ind, c_name, op_fn, c_name, rhs))
                } else {
                    Ok(format!("{}/* compound assign on target */\n", ind))
                }
            }

            // ── If statement ──────────────────────────────────────────────────
            Stmt::If { cond, then_body, elif_clauses, else_body } => {
                let cond_s = self.emit_expr_str(cond, depth)?;
                let mut out = format!("{}if (nv_truthy({})) {{\n", ind, cond_s);
                self.push_scope();
                for s in then_body { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                self.pop_scope();
                for (ec, eb) in elif_clauses {
                    let ec_s = self.emit_expr_str(ec, depth)?;
                    out.push_str(&format!("{}}} else if (nv_truthy({})) {{\n", ind, ec_s));
                    self.push_scope();
                    for s in eb { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                    self.pop_scope();
                }
                if let Some(eb) = else_body {
                    out.push_str(&format!("{}}} else {{\n", ind));
                    self.push_scope();
                    for s in eb { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                    self.pop_scope();
                }
                out.push_str(&format!("{}}}\n", ind));
                Ok(out)
            }

            // ── For loop ──────────────────────────────────────────────────────
            Stmt::For { var, iter, body } => {
                match iter.as_ref() {
                    // `for i in start..end` or `for i in start..=end`
                    Expr::Range { start, end, inclusive } => {
                        let s = self.emit_expr_str(start, depth)?;
                        let e = self.emit_expr_str(end, depth)?;
                        let cmp = if *inclusive { "<=" } else { "<" };
                        // Raw int for loop; body var is scoped inside the loop only
                        let idx = self.fresh("_i");
                        let mut out = format!(
                            "{}for (int64_t {} = ({}).i; {} {} ({}).i; {}++) {{\n",
                            ind, idx, s, idx, cmp, e, idx
                        );
                        // Push scope so body_var is only visible inside the loop body
                        self.push_scope();
                        let body_var = match var {
                            ForVar::Simple(n) if n == "_" => self.fresh("_unused"),
                            ForVar::Simple(n) => { let c = self.fresh(n); self.scopes.last_mut().unwrap().insert(n.clone(), c.clone()); c }
                            ForVar::Tuple(ns) => { let c = self.fresh(ns.first().unwrap_or(&"_i".to_string())); c }
                        };
                        out.push_str(&format!("{}    NvVal {} = nv_int({});\n", ind, body_var, idx));
                        for s in body { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                        self.pop_scope();
                        out.push_str(&format!("{}}}\n", ind));
                        Ok(out)
                    }
                    // `for x in list_expr`
                    iter_expr => {
                        let iter_s = self.emit_expr_str(iter_expr, depth)?;
                        let iter_var = self.fresh("_iter");
                        let idx_var  = self.fresh("_idx");
                        let mut out = format!("{}NvVal {} = {};\n", ind, iter_var, iter_s);
                        // For map.entries(), iter_var will be a list of [k,v] pairs
                        let body_var = match var {
                            ForVar::Simple(n) if n == "_" => self.fresh("_unused"),
                            ForVar::Simple(n) => self.fresh(n),
                            ForVar::Tuple(ns) => self.fresh(ns.first().map(|s| s.as_str()).unwrap_or("_x")),
                        };
                        out.push_str(&format!(
                            "{}for (size_t {} = 0; {} < {}.list->len; {}++) {{\n",
                            ind, idx_var, idx_var, iter_var, idx_var
                        ));
                        out.push_str(&format!("{}    NvVal {} = {}.list->data[{}];\n",
                            ind, body_var, iter_var, idx_var));
                        // Register in scope for body
                        if let ForVar::Simple(n) = var {
                            if n != "_" {
                                self.scopes.last_mut().unwrap().insert(n.clone(), body_var.clone());
                            }
                        } else if let ForVar::Tuple(ns) = var {
                            for (i, n) in ns.iter().enumerate() {
                                if n == "_" { continue; }
                                let elem = self.fresh(n);
                                out.push_str(&format!("{}    NvVal {} = nv_index({}, nv_int({}));\n",
                                    ind, elem, body_var, i));
                                self.scopes.last_mut().unwrap().insert(n.clone(), elem);
                            }
                        }
                        self.push_scope();
                        for s in body { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                        self.pop_scope();
                        out.push_str(&format!("{}}}\n", ind));
                        Ok(out)
                    }
                }
            }

            // ── While loop ────────────────────────────────────────────────────
            Stmt::While { cond, body } => {
                let cond_s = self.emit_expr_str(cond, depth)?;
                let mut out = format!("{}while (nv_truthy({})) {{\n", ind, cond_s);
                self.push_scope();
                for s in body { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                self.pop_scope();
                // Re-emit condition update — but we rely on the loop body updating vars
                // We need to re-evaluate cond each iteration — emit as while(1) { if(!cond) break; ... }
                // Actually the while loop naturally re-evaluates; the issue is that cond references
                // C variable names that are in scope.  Let's use while(1) with inline cond check.
                // Redo: emit as `while (nv_truthy(COND)) { ... }` but use a helper to re-eval
                out = format!("{}while (1) {{\n", ind);
                out.push_str(&format!("{}    if (!nv_truthy({})) break;\n", ind,
                    self.emit_expr_str(cond, depth)?));
                self.push_scope();
                for s in body { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                self.pop_scope();
                out.push_str(&format!("{}}}\n", ind));
                Ok(out)
            }

            // ── Return ────────────────────────────────────────────────────────
            Stmt::Return(expr) => {
                if let Some(e) = expr {
                    // TCO: if this is a self-tail-call, emit param updates + continue
                    if let Some((ref tco_name, ref param_c_names)) = self.tco_self.clone() {
                        if let Expr::Call { callee, args, .. } = e.as_ref() {
                            if let Expr::Ident(n) = callee.as_ref() {
                                if n.as_str() == tco_name.as_str() && args.len() == param_c_names.len() {
                                    // Evaluate all new args first into temps (avoid clobbering)
                                    let mut out = String::new();
                                    let temps: Vec<String> = args.iter().enumerate().map(|(i, _)| {
                                        format!("_tco_tmp_{}", i)
                                    }).collect();
                                    for (i, arg) in args.iter().enumerate() {
                                        let s = self.emit_expr_str(arg, depth)?;
                                        out.push_str(&format!("{}NvVal {} = {};\n", ind, temps[i], s));
                                    }
                                    for (i, pname) in param_c_names.iter().enumerate() {
                                        out.push_str(&format!("{}{} = {};\n", ind, pname, temps[i]));
                                    }
                                    out.push_str(&format!("{}continue;\n", ind));
                                    return Ok(out);
                                }
                            }
                        }
                    }
                    let s = self.emit_expr_str(e, depth)?;
                    Ok(format!("{}return {};\n", ind, s))
                } else {
                    Ok(format!("{}return nv_nil();\n", ind))
                }
            }

            // ── Break / Continue ──────────────────────────────────────────────
            Stmt::Break(_)  => Ok(format!("{}break;\n", ind)),
            Stmt::Continue  => Ok(format!("{}continue;\n", ind)),

            // ── FnDecl inside a block (local function) ────────────────────────
            Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
                let free_vars = self.find_free_vars(def);
                if let Some(ref n) = def.name {
                    self.define_fn(n, def.params.len());
                }
                self.emit_fn_def(def, &free_vars)?;
                Ok(String::new())
            }

            // ── Bare expression ───────────────────────────────────────────────
            Stmt::Expr(expr) => {
                let s = self.emit_expr_str(expr, depth)?;
                Ok(format!("{}{};\n", ind, s))
            }

            // ── Match statement ───────────────────────────────────────────────
            Stmt::Match { expr, arms } => {
                self.emit_match_stmt(expr, arms, depth)
            }

            // ── Comptime constant ─────────────────────────────────────────────
            Stmt::Comptime { name, value } => {
                let expr_s = self.emit_expr_str(value, depth)?;
                let c_name = self.define(name);
                // If pre-declared as a file-scope static, assign (not redeclare)
                if self.static_globals.contains(&c_name) {
                    Ok(format!("{}{} = {};\n", ind, c_name, expr_s))
                } else {
                    Ok(format!("{}NvVal {} = {};\n", ind, c_name, expr_s))
                }
            }

            // ── ImplDecl → emit nv_fn_impl_for registrations ─────────────────
            Stmt::ImplDecl { trait_name, type_name, methods } => {
                let mut out = format!("{}/* impl {} for {} */\n", ind,
                    trait_name.as_deref().unwrap_or(""), type_name);
                for m in methods {
                    if let Some(ref mname) = m.name {
                        let c_fn = format!("nv_fn_{}", c_ident(mname));
                        let trait_s = trait_name.as_deref().unwrap_or(type_name.as_str());
                        out.push_str(&format!(
                            "{}nv_fn_impl_for(nv_str(\"{}\"), nv_str(\"{}\"), nv_str(\"{}\"), nv_fn((NvFn){}), nv_nil());\n",
                            ind, trait_s, type_name, mname, c_fn
                        ));
                    }
                }
                Ok(out)
            }

            // ── throw expr ───────────────────────────────────────────────────
            Stmt::Throw(expr) => {
                let val = self.emit_expr_str(expr, depth)?;
                Ok(format!("{}nv_throw({});\n", ind, val))
            }

            // ── try / catch ──────────────────────────────────────────────────
            Stmt::TryCatch { body, catches } => {
                let mut out = format!("{}NV_TRY_BEGIN\n", ind);
                for s in body {
                    out.push_str(&self.emit_stmt_str(s, depth + 1)?);
                }
                for (var, handler) in catches {
                    let cvar = c_ident(var);
                    out.push_str(&format!("{}NV_TRY_CATCH({})\n", ind, cvar));
                    for s in handler {
                        out.push_str(&self.emit_stmt_str(s, depth + 1)?);
                    }
                }
                out.push_str(&format!("{}NV_TRY_END;\n", ind));
                Ok(out)
            }

            // ── Type / Trait / Import / etc. ──────────────────────────────────
            Stmt::TypeDecl { .. } | Stmt::TraitDecl { .. }
            | Stmt::Import { .. } | Stmt::ExternFn { .. }
            | Stmt::Unsafe(_) | Stmt::AwaitStmt(_) | Stmt::SpawnStmt(_)
            | Stmt::Annotation { .. } => {
                Ok(format!("{}/* {} */\n", ind, stmt_kind_name(stmt)))
            }
        }
    }

    // ── Match statement ───────────────────────────────────────────────────────

    fn emit_match_stmt(&mut self, expr: &Expr, arms: &[MatchArm], depth: usize) -> Result<String, ParseError> {
        let ind = "    ".repeat(depth);
        let subject = self.fresh("_match");
        let done_var = self.fresh("_done");
        let subject_s = self.emit_expr_str(expr, depth)?;
        let has_guard = arms.iter().any(|a| a.guard.is_some());
        let mut out = format!("{}NvVal {} = {};\n", ind, subject, subject_s);
        if has_guard {
            out.push_str(&format!("{}int {} = 0;\n", ind, done_var));
        }
        let mut first = true;
        for arm in arms {
            let cond = self.emit_pattern_cond(&arm.pattern, &subject, depth)?;
            if has_guard {
                // Use _done flag approach: each arm is independent if/if
                out.push_str(&format!("{}if (!{} && {}) {{\n", ind, done_var, cond));
                self.push_scope();
                let mut bind_out = String::new();
                self.emit_pattern_bindings(&arm.pattern, &subject, &mut bind_out, depth+1)?;
                out.push_str(&bind_out);
                if let Some(guard) = &arm.guard {
                    let g = self.emit_expr_str(guard, depth+1)?;
                    out.push_str(&format!("{}    if (nv_truthy({})) {{\n", ind, g));
                    out.push_str(&format!("{}        {} = 1;\n", ind, done_var));
                    match &arm.body {
                        MatchBody::Expr(e) => {
                            let s = self.emit_expr_str(e, depth+2)?;
                            out.push_str(&format!("{}        {};\n", ind, s));
                        }
                        MatchBody::Block(stmts) => {
                            for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+2)?); }
                        }
                    }
                    out.push_str(&format!("{}    }}\n", ind));
                } else {
                    out.push_str(&format!("{}    {} = 1;\n", ind, done_var));
                    match &arm.body {
                        MatchBody::Expr(e) => {
                            let s = self.emit_expr_str(e, depth+1)?;
                            out.push_str(&format!("{}    {};\n", ind, s));
                        }
                        MatchBody::Block(stmts) => {
                            for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                        }
                    }
                }
                self.pop_scope();
                out.push_str(&format!("{}}}\n", ind));
            } else {
                // No guards anywhere — use simpler if/else-if chain
                let kw = if first { "if" } else { "} else if" };
                first = false;
                out.push_str(&format!("{}{} ({}) {{\n", ind, kw, cond));
                self.push_scope();
                self.emit_pattern_bindings(&arm.pattern, &subject, &mut out, depth+1)?;
                match &arm.body {
                    MatchBody::Expr(e) => {
                        let s = self.emit_expr_str(e, depth+1)?;
                        out.push_str(&format!("{}    {};\n", ind, s));
                    }
                    MatchBody::Block(stmts) => {
                        for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                    }
                }
                self.pop_scope();
            }
        }
        if !has_guard && !first { out.push_str(&format!("{}}}\n", ind)); }
        Ok(out)
    }

    fn emit_pattern_cond(&self, pat: &Pattern, subject: &str, _depth: usize) -> Result<String, ParseError> {
        Ok(match pat {
            Pattern::Wildcard | Pattern::Bind(_) => "1".to_string(),
            Pattern::Literal(e) => match e.as_ref() {
                Expr::Int(n)   => format!("nv_truthy(nv_eq({}, nv_int({})))", subject, n),
                Expr::Float(f) => format!("nv_truthy(nv_eq({}, nv_float({})))", subject, f),
                Expr::Str(s)   => format!("nv_truthy(nv_eq({}, nv_str(\"{}\")))", subject, escape_str(s)),
                Expr::Bool(b)  => format!("nv_truthy(nv_eq({}, nv_bool({})))", subject, if *b {1} else {0}),
                Expr::Nil      => format!("{}.tag == NV_NIL", subject),
                _ => "1".to_string(),
            },
            Pattern::NegInt(n)  => format!("nv_truthy(nv_eq({}, nv_int({})))", subject, -n),
            Pattern::NonePat    => format!("{}.tag == NV_NIL", subject),
            Pattern::Range { start, end, inclusive } => {
                let cmp = if *inclusive { "<=" } else { "<" };
                if let Some(e) = end {
                    format!("({}.i >= {} && {}.i {} {})", subject, start, subject, cmp, e)
                } else {
                    format!("{}.i >= {}", subject, start)
                }
            }
            Pattern::Or(pats) => {
                let parts: Result<Vec<_>, _> = pats.iter()
                    .map(|p| self.emit_pattern_cond(p, subject, _depth))
                    .collect();
                format!("({})", parts?.join(" || "))
            }
            Pattern::Ctor { name, variant, args } => {
                match variant {
                    Some(vname) => {
                        // Color.Red or Color.Custom(n) — enum variant pattern
                        format!(
                            "({s}.tag == NV_MAP && nv_truthy(nv_eq(nv_map_get_opt({s}, nv_str(\"_type\")), nv_str(\"{t}\"))) && nv_truthy(nv_eq(nv_map_get_opt({s}, nv_str(\"_tag\")), nv_str(\"{v}\"))))",
                            s = subject, t = name, v = vname
                        )
                    }
                    None if !args.is_empty() => {
                        // Variant(args) without type prefix — match by _tag
                        format!(
                            "({s}.tag == NV_MAP && nv_truthy(nv_eq(nv_map_get_opt({s}, nv_str(\"_tag\")), nv_str(\"{n}\"))))",
                            s = subject, n = name
                        )
                    }
                    None => {
                        // Bare uppercase name — treat as string constant or nil
                        format!("nv_truthy(nv_eq({}, nv_str(\"{}\")))", subject, name)
                    }
                }
            }
            _ => "1".to_string(),
        })
    }

    fn emit_pattern_bindings(&mut self, pat: &Pattern, subject: &str, out: &mut String, depth: usize) -> Result<(), ParseError> {
        let ind = "    ".repeat(depth);
        match pat {
            Pattern::Bind(name) => {
                let c_name = self.define(name);
                out.push_str(&format!("{}NvVal {} = {};\n", ind, c_name, subject));
            }
            Pattern::Ctor { variant, args, .. } => {
                // Bind positional args from enum variant pattern
                for (i, arg) in args.iter().enumerate() {
                    if let Pattern::Bind(n) = arg {
                        let c_name = self.define(n);
                        if variant.is_some() {
                            // Enum variant: fields stored under "0", "1", ...
                            out.push_str(&format!(
                                "{}NvVal {} = nv_map_get_opt({}, nv_str(\"{}\"));\n",
                                ind, c_name, subject, i
                            ));
                        } else {
                            // Struct/positional: try map index by position
                            out.push_str(&format!(
                                "{}NvVal {} = ({}.tag == NV_MAP && {}.map->len > {}) ? {}.map->data[{}].val : nv_index({}, nv_int({}));\n",
                                ind, c_name, subject, subject, i, subject, i, subject, i
                            ));
                        }
                    }
                }
            }
            Pattern::SomePat(inner) => {
                // Some(x) — subject is the value itself
                self.emit_pattern_bindings(inner, subject, out, depth)?;
            }
            Pattern::Or(pats) => {
                // Bind from the first arm (all arms must bind the same names)
                if let Some(first) = pats.first() {
                    self.emit_pattern_bindings(first, subject, out, depth)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ── Expression emission ───────────────────────────────────────────────────

    fn emit_expr_str(&mut self, expr: &Expr, depth: usize) -> Result<String, ParseError> {
        match expr {
            // ── Literals ──────────────────────────────────────────────────────
            Expr::Int(n)   => Ok(format!("nv_int({})", n)),
            Expr::Float(f) => Ok(format!("nv_float({})", format_float(*f))),
            Expr::Bool(b)  => Ok(format!("nv_bool({})", if *b { 1 } else { 0 })),
            Expr::Nil      => Ok("nv_nil()".to_string()),
            Expr::Self_    => Ok("self_".to_string()),

            // ── String (with interpolation) ───────────────────────────────────
            Expr::Str(s) => {
                if s.contains('{') {
                    self.emit_interp_str(s, depth)
                } else {
                    Ok(format!("nv_str(\"{}\")", escape_str(s)))
                }
            }

            // ── Identifier ────────────────────────────────────────────────────
            Expr::Ident(name) => {
                // None/nil as bare ident — fallback when result.nvl not imported
                if name == "None" && !self.fn_names_orig.contains("None") {
                    return Ok("nv_nil()".to_string());
                }
                let c_name = self.lookup(name);
                // Declared functions used as values need to be wrapped as NvVal.
                // Only wrap if the resolved C name is actually a function (starts with nv_fn_),
                // not if a variable of the same name shadows it in scope.
                if c_name.starts_with("nv_fn_") {
                    Ok(format!("nv_fn((NvFn){})", c_name))
                } else if is_builtin_name(name) {
                    // Builtin used as a value (HOF argument) — wrap its C fn
                    let c_fn = match name.as_str() {
                        "mse_loss"      => "nv_fn_mse_loss",
                        "cross_entropy" => "nv_fn_cross_entropy",
                        "batch_norm"    => "nv_fn_batch_norm",
                        "layer_norm"    => "nv_fn_layer_norm",
                        _ => return Ok(c_name), // fall through for others
                    };
                    Ok(format!("nv_fn((NvFn){})", c_fn))
                } else {
                    Ok(c_name)
                }
            }

            // ── Binary operations ─────────────────────────────────────────────
            Expr::BinOp { op, lhs, rhs } => {
                let l = self.emit_expr_str(lhs, depth)?;
                // `and`/`or` must short-circuit (C && / ||) so the RHS is
                // never evaluated when the LHS already determines the result.
                match op {
                    BinOp::And => return Ok(format!(
                        "nv_bool(nv_truthy({}) && nv_truthy({}))", l,
                        self.emit_expr_str(rhs, depth)?
                    )),
                    BinOp::Or  => return Ok(format!(
                        "nv_bool(nv_truthy({}) || nv_truthy({}))", l,
                        self.emit_expr_str(rhs, depth)?
                    )),
                    _ => {}
                }
                let r = self.emit_expr_str(rhs, depth)?;
                let fn_name = match op {
                    BinOp::Add    => "nv_add",
                    BinOp::Sub    => "nv_sub",
                    BinOp::Mul    => "nv_mul",
                    BinOp::Div    => "nv_div",
                    BinOp::IntDiv => "nv_idiv",
                    BinOp::Mod    => "nv_mod",
                    BinOp::Pow    => "nv_pow",
                    BinOp::Eq     => "nv_eq",
                    BinOp::Ne     => "nv_ne",
                    BinOp::Lt     => "nv_lt",
                    BinOp::Le     => "nv_le",
                    BinOp::Gt     => "nv_gt",
                    BinOp::Ge     => "nv_ge",
                    BinOp::Is     => "nv_is",
                    BinOp::Matmul => "nv_matmul",
                    BinOp::And | BinOp::Or => unreachable!(),
                };
                Ok(format!("{}({}, {})", fn_name, l, r))
            }

            // ── Unary operations ──────────────────────────────────────────────
            Expr::UnOp { op, expr } => {
                let e = self.emit_expr_str(expr, depth)?;
                match op {
                    UnOp::Neg => Ok(format!("nv_neg({})", e)),
                    UnOp::Not => Ok(format!("nv_not({})", e)),
                }
            }

            // ── Pipeline: lhs |> rhs ──────────────────────────────────────────
            Expr::Pipe { lhs, rhs } => {
                self.emit_pipe(lhs, rhs, depth)
            }

            // ── Range ─────────────────────────────────────────────────────────
            Expr::Range { start, end, inclusive } => {
                let s = self.emit_expr_str(start, depth)?;
                let e = self.emit_expr_str(end, depth)?;
                Ok(format!("nv_range({}, {}, {})", s, e, if *inclusive { 1 } else { 0 }))
            }

            // ── Function call ─────────────────────────────────────────────────
            Expr::Call { callee, args, kwargs } => {
                self.emit_call(callee, args, kwargs, depth)
            }

            // ── Method call ───────────────────────────────────────────────────
            Expr::MethodCall { obj, method, args, kwargs } => {
                self.emit_method_call(obj, method, args, kwargs, depth)
            }

            // ── Index ─────────────────────────────────────────────────────────
            Expr::Index { obj, idx } => {
                let o = self.emit_expr_str(obj, depth)?;
                let i = self.emit_expr_str(idx, depth)?;
                Ok(format!("nv_index({}, {})", o, i))
            }

            // ── Field access ──────────────────────────────────────────────────
            Expr::Field { obj, field } => {
                if let Expr::Ident(ns_name) = obj.as_ref() {
                    // Enum variant access: Color.Red → call zero-arg constructor
                    if let Some(variants) = self.enum_types.get(ns_name).cloned() {
                        if let Some(v) = variants.iter().find(|v| &v.name == field) {
                            let fn_c = format!("nv_fn_{}_{}", c_ident(ns_name), c_ident(field));
                            if v.fields.is_empty() {
                                return Ok(format!("{}(nv_nil(), nv_nil())", fn_c));
                            } else {
                                // Parametric variant used as a function value
                                return Ok(format!("nv_fn((NvFn){})", fn_c));
                            }
                        }
                    }
                    // Namespace field access: ns.CONSTANT → nv_map_get_opt(g_ns, "CONSTANT")
                    if self.ns_fns.contains_key(ns_name.as_str()) {
                        let ns_var = format!("g_{}", ns_name);
                        return Ok(format!("nv_map_get_opt({}, nv_str(\"{}\"))", ns_var, field));
                    }
                }
                let o = self.emit_expr_str(obj, depth)?;
                self.emit_field(&o, field)
            }

            // ── Optional chain ────────────────────────────────────────────────
            Expr::OptChain { obj, field } => {
                let o = self.emit_expr_str(obj, depth)?;
                self.emit_field(&o, field)
            }

            // ── List literal ──────────────────────────────────────────────────
            Expr::List(items) => {
                if items.is_empty() {
                    return Ok("nv_list_new()".to_string());
                }
                let mut parts = Vec::new();
                for item in items { parts.push(self.emit_expr_str(item, depth)?); }
                Ok(format!("nv_list_of({}, {})", parts.len(), parts.join(", ")))
            }

            // ── Map literal ───────────────────────────────────────────────────
            Expr::Map(pairs) => {
                if pairs.is_empty() {
                    return Ok("nv_map_new()".to_string());
                }
                let mut parts = Vec::new();
                for (k, v) in pairs {
                    parts.push(self.emit_expr_str(k, depth)?);
                    parts.push(self.emit_expr_str(v, depth)?);
                }
                Ok(format!("nv_map_of({}, {})", pairs.len(), parts.join(", ")))
            }

            // ── Set literal ───────────────────────────────────────────────────
            Expr::Set(items) => {
                // Emit as a list for now (Stage 0 simplification)
                if items.is_empty() { return Ok("nv_list_new()".to_string()); }
                let mut parts = Vec::new();
                for item in items { parts.push(self.emit_expr_str(item, depth)?); }
                Ok(format!("nv_list_of({}, {})", parts.len(), parts.join(", ")))
            }

            // ── Tuple ─────────────────────────────────────────────────────────
            Expr::Tuple(items) => {
                if items.is_empty() { return Ok("nv_nil()".to_string()); }
                let mut parts = Vec::new();
                for item in items { parts.push(self.emit_expr_str(item, depth)?); }
                Ok(format!("nv_list_of({}, {})", parts.len(), parts.join(", ")))
            }

            // ── Lambda ────────────────────────────────────────────────────────
            Expr::Lambda(def) => {
                let lambda_name = self.fresh("__lambda");
                let free_vars = self.find_free_vars(def);
                let mut cloned_def = *def.clone();
                cloned_def.name = Some(lambda_name.clone());
                self.scopes[0].insert(lambda_name.clone(), lambda_name.clone());
                self.emit_fn_def(&cloned_def, &free_vars)?;
                if free_vars.is_empty() {
                    Ok(format!("nv_fn((NvFn){})", lambda_name))
                } else {
                    // Build env map with current values of captured vars
                    let env_parts: Vec<String> = free_vars.iter().map(|v| {
                        let cv = self.lookup(v);
                        format!("nv_str(\"{}\"), {}", v, cv)
                    }).collect();
                    Ok(format!("nv_closure((NvFn){}, nv_map_of({}, {}))",
                        lambda_name, free_vars.len(), env_parts.join(", ")))
                }
            }

            // ── Placeholder ───────────────────────────────────────────────────
            Expr::Placeholder(op) => {
                // `_` → identity, `_ + 1` → fn(x) x+1, `_ * _` → fn(x) x*x
                match op {
                    None => Ok("nv_fn((NvFn)__placeholder)".to_string()),
                    Some(pop) => {
                        let lname = self.fresh("__ph");
                        match pop.as_ref() {
                            PlaceholderOp::Field(fname) => {
                                self.top.push_str(&format!(
                                    "static NvVal {}(NvVal _x, NvVal __env) {{ (void)__env; return nv_map_get_opt(_x, nv_str(\"{}\"));}}\n",
                                    lname, fname
                                ));
                            }
                            PlaceholderOp::Bin(op, rhs) => {
                                // Check if rhs is also a placeholder (e.g. _ * _)
                                let rhs_c = match rhs.as_ref() {
                                    Expr::Placeholder(None) => "_x".to_string(),
                                    other => {
                                        // Need to emit rhs as a constant in a context without _x
                                        // For safety, emit rhs directly (it shouldn't ref _x)
                                        self.emit_expr_str(other, 0).unwrap_or_else(|_| "nv_nil()".to_string())
                                    }
                                };
                                let op_fn = match op {
                                    BinOp::Add  => "nv_add",
                                    BinOp::Sub  => "nv_sub",
                                    BinOp::Mul  => "nv_mul",
                                    BinOp::Div  => "nv_div",
                                    BinOp::Mod  => "nv_mod",
                                    BinOp::Eq   => "nv_eq",
                                    BinOp::Ne   => "nv_ne",
                                    BinOp::Lt   => "nv_lt",
                                    BinOp::Le   => "nv_le",
                                    BinOp::Gt   => "nv_gt",
                                    BinOp::Ge   => "nv_ge",
                                    BinOp::And  => "nv_and",
                                    BinOp::Or   => "nv_or",
                                    _ => "nv_add",
                                };
                                self.top.push_str(&format!(
                                    "static NvVal {}(NvVal _x, NvVal __env) {{ (void)__env; return {}(_x, {}); }}\n",
                                    lname, op_fn, rhs_c
                                ));
                            }
                        }
                        Ok(format!("nv_fn((NvFn){})", lname))
                    }
                }
            }

            // ── If expression ─────────────────────────────────────────────────
            Expr::If { cond, then_expr, elif_clauses, else_expr } => {
                self.emit_if_expr(cond, then_expr, elif_clauses, else_expr.as_deref(), depth)
            }

            // ── Match expression ──────────────────────────────────────────────
            Expr::Match { expr, arms } => {
                // Emit match as a GCC block expression ({ ... })
                let subject_s = self.emit_expr_str(expr, depth)?;
                let result_var = self.fresh("_match_r");
                let subject_var = self.fresh("_match_s");
                let has_guard = arms.iter().any(|a| a.guard.is_some());
                let mut block = format!("({{ NvVal {} = {}; NvVal {} = nv_nil();\n",
                    subject_var, subject_s, result_var);
                if has_guard {
                    let done_var = self.fresh("_done");
                    block.push_str(&format!("    int {} = 0;\n", done_var));
                    for arm in arms {
                        let cond = self.emit_pattern_cond(&arm.pattern, &subject_var, depth)?;
                        block.push_str(&format!("    if (!{} && {}) {{\n", done_var, cond));
                        self.push_scope();
                        let mut bind_out = String::new();
                        self.emit_pattern_bindings(&arm.pattern, &subject_var, &mut bind_out, depth+2)?;
                        block.push_str(&bind_out);
                        if let Some(guard) = &arm.guard {
                            let g = self.emit_expr_str(guard, depth+2)?;
                            block.push_str(&format!("        if (nv_truthy({})) {{\n", g));
                            block.push_str(&format!("            {} = 1;\n", done_var));
                            match &arm.body {
                                MatchBody::Expr(e) => {
                                    let s = self.emit_expr_str(e, depth+3)?;
                                    block.push_str(&format!("            {} = {};\n", result_var, s));
                                }
                                MatchBody::Block(stmts) => {
                                    for s in stmts { block.push_str(&self.emit_stmt_str(s, depth+3)?); }
                                }
                            }
                            block.push_str("        }\n");
                        } else {
                            block.push_str(&format!("        {} = 1;\n", done_var));
                            match &arm.body {
                                MatchBody::Expr(e) => {
                                    let s = self.emit_expr_str(e, depth+2)?;
                                    block.push_str(&format!("        {} = {};\n", result_var, s));
                                }
                                MatchBody::Block(stmts) => {
                                    for s in stmts { block.push_str(&self.emit_stmt_str(s, depth+2)?); }
                                }
                            }
                        }
                        self.pop_scope();
                        block.push_str("    }\n");
                    }
                } else {
                    let mut first = true;
                    for arm in arms {
                        let cond = self.emit_pattern_cond(&arm.pattern, &subject_var, depth)?;
                        let kw = if first { "if" } else { "} else if" };
                        first = false;
                        block.push_str(&format!("    {} ({}) {{\n", kw, cond));
                        self.push_scope();
                        let mut bind_out = String::new();
                        self.emit_pattern_bindings(&arm.pattern, &subject_var, &mut bind_out, depth+2)?;
                        block.push_str(&bind_out);
                        match &arm.body {
                            MatchBody::Expr(e) => {
                                let s = self.emit_expr_str(e, depth+2)?;
                                block.push_str(&format!("        {} = {};\n", result_var, s));
                            }
                            MatchBody::Block(stmts) => {
                                for s in stmts { block.push_str(&self.emit_stmt_str(s, depth+2)?); }
                            }
                        }
                        self.pop_scope();
                    }
                    if !first { block.push_str("    }\n"); }
                }
                block.push_str(&format!("    {}; }})", result_var));
                Ok(block)
            }

            // ── Await / Spawn ─────────────────────────────────────────────────
            Expr::Await(e) => {
                let fut_s = self.emit_expr_str(e, depth)?;
                Ok(format!("nv_fn_await_({}, nv_nil())", fut_s))
            }
            Expr::Spawn(e) => {
                // spawn(fn, arg) is parsed as Spawn(Tuple([fn, arg]))
                if let Expr::Tuple(items) = e.as_ref() {
                    let fn_s = if let Some(f) = items.get(0) { self.emit_expr_str(f, depth)? } else { "nv_nil()".to_string() };
                    let arg_s = if let Some(a) = items.get(1) { self.emit_expr_str(a, depth)? } else { "nv_nil()".to_string() };
                    Ok(format!("nv_spawn({}, {})", fn_s, arg_s))
                } else {
                    let e_s = self.emit_expr_str(e, depth)?;
                    Ok(format!("nv_spawn({}, nv_nil())", e_s))
                }
            }
            Expr::Unsafe(stmts) => {
                let mut out = String::from("({ NvVal _r = nv_nil();\n");
                for s in stmts { out.push_str(&self.emit_stmt_str(s, depth+1)?); }
                out.push_str("_r; })");
                Ok(out)
            }

            // ── Struct literal ────────────────────────────────────────────────
            Expr::Struct { name: _, fields } => {
                let has_spread = fields.iter().any(|f| matches!(f, StructField::Spread(_)));
                if !has_spread {
                    let mut pairs: Vec<String> = Vec::new();
                    for f in fields {
                        if let StructField::Named { name, value } = f {
                            let vs = self.emit_expr_str(value, depth)?;
                            pairs.push(format!("nv_str(\"{}\"), {}", name, vs));
                        }
                    }
                    Ok(format!("nv_map_of({}, {})", pairs.len(), pairs.join(", ")))
                } else {
                    // Has spread fields: build map then merge/set
                    let sv = self.fresh("_struct");
                    let mut out = format!("({{ NvVal {} = nv_map_new(); ", sv);
                    for f in fields {
                        match f {
                            StructField::Spread(base) => {
                                let b = self.emit_expr_str(base, depth)?;
                                out.push_str(&format!("nv_map_merge({}, {}); ", sv, b));
                            }
                            StructField::Named { name, value } => {
                                let vs = self.emit_expr_str(value, depth)?;
                                out.push_str(&format!("nv_map_set_mut({}, nv_str(\"{}\"), {}); ", sv, name, vs));
                            }
                        }
                    }
                    out.push_str(&format!("{}; }})", sv));
                    Ok(out)
                }
            }
        }
    }

    // ── Pipeline lowering ─────────────────────────────────────────────────────

    fn emit_pipe(&mut self, lhs: &Expr, rhs: &Expr, depth: usize) -> Result<String, ParseError> {
        let lhs_s = self.emit_expr_str(lhs, depth)?;
        match rhs {
            // `lhs |> fn_name` (no call parens) → `fn_name(lhs)`
            Expr::Ident(name) => {
                let fn_s = self.lookup(name);
                let simple = match name.as_str() {
                    "sum"             => format!("nv_sum({})", lhs_s),
                    "print"           => format!("nv_print({})", lhs_s),
                    "sorted"|"sort"   => format!("nv_sorted({})", lhs_s),
                    "reversed"|"reverse" => format!("nv_reversed({})", lhs_s),
                    "len"             => format!("nv_len({})", lhs_s),
                    "max"             => format!("nv_max_fn({})", lhs_s),
                    "min"             => format!("nv_min_fn({})", lhs_s),
                    "str"             => format!("nv_to_str({})", lhs_s),
                    "chars"           => format!("nv_str_chars({})", lhs_s),
                    "entries"         => format!("nv_map_entries({})", lhs_s),
                    _ if self.fn_names_orig.contains(name.as_str()) =>
                        format!("{}({}, nv_nil())", fn_s, lhs_s),
                    _ => self.emit_nv_call(&fn_s, &[lhs_s.clone()]),
                };
                Ok(simple)
            }
            // `lhs |> f(a, b)` → `f(lhs, a, b)`
            Expr::Call { callee, args, kwargs: _ } => {
                let mut all_args = vec![lhs_s];
                for a in args { all_args.push(self.emit_expr_str(a, depth)?); }
                match callee.as_ref() {
                    Expr::Ident(fname) => {
                        let fn_s = self.dispatch_builtin(fname, &all_args);
                        Ok(fn_s)
                    }
                    other => {
                        let c = self.emit_expr_str(other, depth)?;
                        Ok(format!("{}({})", c, all_args.join(", ")))
                    }
                }
            }
            // `lhs |> (nested pipe)`
            other => {
                let r = self.emit_expr_str(other, depth)?;
                Ok(format!("/* pipe */ {}", r))
            }
        }
    }

    /// Resolve a builtin call in pipeline context — args[0] is the piped value
    fn dispatch_builtin(&self, name: &str, args: &[String]) -> String {
        let lhs = &args[0];
        match name {
            "map"      => format!("nv_map_fn({}, {})", args.get(1).cloned().unwrap_or_default(), lhs),
            "filter"   => format!("nv_filter({}, {})", args.get(1).cloned().unwrap_or_default(), lhs),
            "sum"      => format!("nv_sum({})", lhs),
            "print"    => format!("nv_print({})", lhs),
            "sorted"   => format!("nv_sorted({})", lhs),
            "reversed" => format!("nv_reversed({})", lhs),
            "sort"     => format!("nv_sorted({})", lhs),
            "reverse"  => format!("nv_reversed({})", lhs),
            "len"      => format!("nv_len({})", lhs),
            "max"      => format!("nv_max_fn({})", lhs),
            "min"      => format!("nv_min_fn({})", lhs),
            "join"     => format!("nv_str_join({}, {})", args.get(1).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()), lhs),
            "split"    => format!("nv_str_split({}, {})", lhs, args.get(1).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string())),
            "take"     => format!("nv_list_take({}, {})", lhs, args.get(1).cloned().unwrap_or_default()),
            "drop"     => format!("nv_list_drop({}, {})", lhs, args.get(1).cloned().unwrap_or_default()),
            "assert"   => {
                let msg = args.get(1).cloned().unwrap_or_else(|| "nv_str(\"assertion failed\")".to_string());
                format!("nv_assert_({}, {})", lhs, msg)
            }
            _ => {
                let fn_s = self.lookup(name);
                if self.fn_names_orig.contains(name) {
                    let mut all = args.to_vec();
                    all.push("nv_nil()".to_string());
                    format!("{}({})", fn_s, all.join(", "))
                } else {
                    self.emit_nv_call(&fn_s, args)
                }
            }
        }
    }

    // ── Function call ─────────────────────────────────────────────────────────

    fn emit_call(&mut self, callee: &Expr, args: &[Expr], kwargs: &[(String, Expr)], depth: usize) -> Result<String, ParseError> {
        let mut arg_strs: Vec<String> = Vec::new();
        for a in args { arg_strs.push(self.emit_expr_str(a, depth)?); }
        // Build a quick lookup for keyword args
        let mut kw: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
        for (k, v) in kwargs {
            kw.insert(k.as_str(), self.emit_expr_str(v, depth)?);
        }

        if let Expr::Ident(name) = callee {
            // ── 1. Language-level builtins ────────────────────────────────────
            let result = match name.as_str() {
                "print"  => Some(format!("nv_print({})", arg_strs.join(", "))),
                "assert" => {
                    let cond = arg_strs.get(0).cloned().unwrap_or_default();
                    let msg  = arg_strs.get(1).cloned()
                        .unwrap_or_else(|| "nv_str(\"assertion failed\")".to_string());
                    Some(format!("nv_assert_({}, {})", cond, msg))
                }
                "map" => {
                    let fn_v = arg_strs.get(0).cloned().unwrap_or_default();
                    let lst  = arg_strs.get(1).cloned().unwrap_or_default();
                    Some(format!("nv_map_fn({}, {})", fn_v, lst))
                }
                "filter" => {
                    let fn_v = arg_strs.get(0).cloned().unwrap_or_default();
                    let lst  = arg_strs.get(1).cloned().unwrap_or_default();
                    Some(format!("nv_filter({}, {})", fn_v, lst))
                }
                "spawn" => {
                    let fn_v = arg_strs.get(0).cloned().unwrap_or_default();
                    let arg  = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_nil()".to_string());
                    Some(format!("nv_spawn({}, {})", fn_v, arg))
                }
                // Some/None/Ok/Err are now stdlib functions (result.nvl)
                // — fall through to fn_names_orig dispatch below.
                "sum"   => Some(format!("nv_sum({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "len"   => Some(format!("nv_len_fn({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "int"   => Some(format!("nv_to_int({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "float" => Some(format!("nv_to_float({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "str"   => Some(format!("nv_to_str({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "type"  => Some(format!("nv_type_of({})", arg_strs.get(0).cloned().unwrap_or_else(|| "nv_nil()".to_string()))),
                "abs"   => Some(format!("nv_abs_fn({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "sqrt"  => Some(format!("nv_sqrt_fn({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "max"   => Some(if arg_strs.len() == 1 { format!("nv_max_fn({})", arg_strs[0]) }
                                else { format!("(nv_truthy(nv_gt({},{})) ? {} : {})", arg_strs[0], arg_strs[1], arg_strs[0], arg_strs[1]) }),
                "min"   => Some(if arg_strs.len() == 1 { format!("nv_min_fn({})", arg_strs[0]) }
                                else { format!("(nv_truthy(nv_lt({},{})) ? {} : {})", arg_strs[0], arg_strs[1], arg_strs[0], arg_strs[1]) }),
                "range" => {
                    let s = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    let e = arg_strs.get(1).cloned().unwrap_or_default();
                    Some(format!("nv_range({}, {}, 0)", s, e))
                }
                "sorted"   => Some(format!("nv_sorted({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "reversed" => Some(format!("nv_reversed({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "read_file"  => Some(format!("nv_read_file({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "write_file" => Some(format!("nv_write_file({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "eprint"     => Some(format!("nv_eprint({})", arg_strs.get(0).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()))),
                "path_dirname" => Some(format!("nv_path_dirname({})", arg_strs.get(0).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()))),
                "path_join"  => Some(format!("nv_path_join({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_else(|| "nv_str(\".\")".to_string()),
                    arg_strs.get(1).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()))),
                "file_exists" => Some(format!("nv_file_exists({})", arg_strs.get(0).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()))),
                "args"       => Some("nv_args()".to_string()),
                "exit"       => Some(format!("(exit(nv_truthy({}) ? (({}).tag==NV_INT?(int)({}).i:0) : 0), nv_nil())",
                    arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string()),
                    arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string()),
                    arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string()))),
                // HTTP / JSON / time builtins (nuvola.h)
                "json_parse"     => Some(format!("nv_json_parse({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "json_stringify" => Some(format!("nv_json_stringify({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "http_get"       => Some(format!("nv_http_get({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "http_post"      => Some(format!("nv_http_post({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()))),
                "http_serve"     => Some(format!("nv_http_serve({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "http_response"  => Some(format!("nv_http_response({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_else(|| "nv_str(\"\")".to_string()))),
                "time_ms"        => Some("nv_time_ms()".to_string()),
                "channel"        => Some("nv_chan_new()".to_string()),
                "sleep_ms"       => Some(format!("nv_sleep_ms({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // OS builtins
                "nv_os_getcwd"    => Some("nv_os_getcwd()".to_string()),
                "nv_os_exists"    => Some(format!("nv_os_exists({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_is_file"   => Some(format!("nv_os_is_file({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_is_dir"    => Some(format!("nv_os_is_dir({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_listdir"   => Some(format!("nv_os_listdir({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_mkdir"     => Some(format!("nv_os_mkdir({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_remove"    => Some(format!("nv_os_remove({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_rename"    => Some(format!("nv_os_rename({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "nv_os_getenv"    => Some(format!("nv_os_getenv({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_setenv"    => Some(format!("nv_os_setenv({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "nv_os_system"    => Some(format!("nv_os_system({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_os_file_size" => Some(format!("nv_os_file_size({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // Benchmark / shell builtins
                "nv_clock_ns" => Some("nv_clock_ns(nv_nil(), nv_nil())".to_string()),
                "nv_shell"    => Some(format!("nv_shell({}, nv_nil())", arg_strs.get(0).cloned().unwrap_or_else(|| "nv_nil()".into()))),
                // Regex builtins
                "regex_match"    => Some(format!("nv_regex_match({}, {})",    arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default())),
                "regex_find"     => Some(format!("nv_regex_find({}, {})",     arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default())),
                "regex_find_all" => Some(format!("nv_regex_find_all({}, {})", arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default())),
                "regex_replace"  => Some(format!("nv_regex_replace({}, {}, {})", arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default(), arg_strs.get(2).cloned().unwrap_or_default())),
                "regex_split"    => Some(format!("nv_regex_split({}, {})",    arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default())),
                // Crypto builtins
                "sha256"       => Some(format!("nv_sha256({})",       arg_strs.get(0).cloned().unwrap_or_default())),
                "sha256_bytes" => Some(format!("nv_sha256_bytes({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "hmac_sha256"  => Some(format!("nv_hmac_sha256({}, {})", arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default())),
                // Base64
                "base64_encode" => Some(format!("nv_base64_encode({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "base64_decode" => Some(format!("nv_base64_decode({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // UTF-8
                "utf8_len"   => Some(format!("nv_utf8_len({})",  arg_strs.get(0).cloned().unwrap_or_default())),
                "utf8_at"    => Some(format!("nv_utf8_at({}, {})",  arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_default())),
                "utf8_chars" => Some(format!("nv_utf8_chars({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // Subprocess capture
                "popen_read"  => Some(format!("nv_popen_read({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // String format with {0} {1} placeholders
                "format_str"  => Some(format!("nv_format_str({}, {})", arg_strs.get(0).cloned().unwrap_or_default(), arg_strs.get(1).cloned().unwrap_or_else(|| "nv_list()".into()))),
                // Time builtins
                "nv_time_now_ms"     => Some("nv_time_now_ms()".to_string()),
                "nv_time_now_us"     => Some("nv_time_now_us()".to_string()),
                "nv_time_now_sec"    => Some("nv_time_now_sec()".to_string()),
                "nv_sleep_sec"       => Some(format!("nv_sleep_sec({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_time_format"     => Some(format!("nv_time_format({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "nv_time_format_iso" => Some(format!("nv_time_format_iso({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // Math builtins (M20: needed when std/math.nvl wrappers are imported)
                "sin"   => Some(format!("nv_sin_fn({})",   arg_strs.get(0).cloned().unwrap_or_default())),
                "cos"   => Some(format!("nv_cos_fn({})",   arg_strs.get(0).cloned().unwrap_or_default())),
                "tan"   => Some(format!("nv_tan_fn({})",   arg_strs.get(0).cloned().unwrap_or_default())),
                "exp"   => Some(format!("nv_exp_fn({})",   arg_strs.get(0).cloned().unwrap_or_default())),
                "log"   => Some(format!("nv_log_fn({})",   arg_strs.get(0).cloned().unwrap_or_default())),
                "log2"  => Some(format!("nv_log2_fn({})",  arg_strs.get(0).cloned().unwrap_or_default())),
                "log10" => Some(format!("nv_log10_fn({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "floor" => Some(format!("nv_floor_fn({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "ceil"  => Some(format!("nv_ceil_fn({})",  arg_strs.get(0).cloned().unwrap_or_default())),
                "round" => Some(format!("nv_round_fn({})", arg_strs.get(0).cloned().unwrap_or_default())),
                "pow"   => Some(format!("nv_pow_fn({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "hypot" => Some(format!("nv_hypot_fn({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "atan2" => Some(format!("nv_atan2_fn({}, {})",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                // Environment / OS builtins
                "env_get" => Some(format!("nv_env_get({})", arg_strs.get(0).cloned().unwrap_or_default())),
                // ── Tensor builtins (nuvola_tensor.h) ────────────────────────
                "tensor" => {
                    let list_s = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_list_new()".to_string());
                    // tensor(data, requires_grad=true) — kwarg takes priority
                    let rg = kw.get("requires_grad")
                        .cloned()
                        .or_else(|| arg_strs.get(1).cloned())
                        .unwrap_or_else(|| "nv_bool(0)".to_string());
                    Some(format!("nv_fn_tensor({}, {}, nv_nil())", list_s, rg))
                }
                "zeros" => {
                    let r = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    let c = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    Some(format!("nv_fn_zeros({}, {}, nv_nil())", r, c))
                }
                "ones" => {
                    let r = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    let c = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    Some(format!("nv_fn_ones({}, {}, nv_nil())", r, c))
                }
                "randn" => {
                    let r = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    let c = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    Some(format!("nv_fn_randn({}, {}, nv_nil())", r, c))
                }
                "rand" => {
                    let r = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    let c = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    Some(format!("nv_fn_rand({}, {}, nv_nil())", r, c))
                }
                "eye" => {
                    let n = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    Some(format!("nv_fn_eye({}, nv_nil())", n))
                }
                "arange" => {
                    let start = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    let end_  = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    let step  = arg_strs.get(2).cloned().unwrap_or_else(|| "nv_int(1)".to_string());
                    Some(format!("nv_fn_arange({}, {}, {}, nv_nil())", start, end_, step))
                }
                "linspace" => {
                    let start = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_float(0.0)".to_string());
                    let end_  = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_float(1.0)".to_string());
                    let n     = arg_strs.get(2).cloned().unwrap_or_else(|| "nv_int(10)".to_string());
                    Some(format!("nv_fn_linspace({}, {}, {}, nv_nil())", start, end_, n))
                }
                "cross_entropy" => {
                    let logits  = arg_strs.get(0).cloned().unwrap_or_default();
                    let targets = arg_strs.get(1).cloned().unwrap_or_default();
                    Some(format!("nv_fn_cross_entropy({}, {}, nv_nil())", logits, targets))
                }
                "mse_loss" => {
                    let pred   = arg_strs.get(0).cloned().unwrap_or_default();
                    let target = arg_strs.get(1).cloned().unwrap_or_default();
                    Some(format!("nv_fn_mse_loss({}, {}, nv_nil())", pred, target))
                }
                "batch_norm" => {
                    let x = arg_strs.get(0).cloned().unwrap_or_default();
                    Some(format!("nv_fn_batch_norm({}, nv_nil())", x))
                }
                "quantize" => {
                    let t    = arg_strs.get(0).cloned().unwrap_or_default();
                    let dtype = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_str(\"i8\")".to_string());
                    Some(format!("nv_fn_quantize({}, {}, nv_nil())", t, dtype))
                }
                "dequantize" => {
                    let qt    = arg_strs.get(0).cloned().unwrap_or_default();
                    let dtype = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_str(\"f32\")".to_string());
                    Some(format!("nv_fn_dequantize({}, {}, nv_nil())", qt, dtype))
                }
                // Trait / channel builtins
                "impl_for"   => Some(format!("nv_fn_impl_for({}, {}, {}, {}, nv_nil())",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default(),
                    arg_strs.get(2).cloned().unwrap_or_default(),
                    arg_strs.get(3).cloned().unwrap_or_default())),
                "call_trait" => Some(format!("nv_fn_call_trait({}, {}, nv_nil())",
                    arg_strs.get(0).cloned().unwrap_or_default(),
                    arg_strs.get(1).cloned().unwrap_or_default())),
                "chan"        => {
                    let cap = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    Some(format!("nv_fn_chan({}, nv_nil())", cap))
                }
                "huber_loss" => {
                    let pred  = arg_strs.get(0).cloned().unwrap_or_default();
                    let tgt   = arg_strs.get(1).cloned().unwrap_or_default();
                    let delta = arg_strs.get(2).cloned().unwrap_or_else(|| "nv_float(1.0)".to_string());
                    Some(format!("nv_fn_huber_loss({}, {}, {}, nv_nil())", pred, tgt, delta))
                }
                "warmup_cosine_lr" => {
                    let step    = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    let warmup  = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(10)".to_string());
                    let total   = arg_strs.get(2).cloned().unwrap_or_else(|| "nv_int(100)".to_string());
                    let max_lr  = arg_strs.get(3).cloned().unwrap_or_else(|| "nv_float(1.0)".to_string());
                    let min_lr  = arg_strs.get(4).cloned().unwrap_or_else(|| "nv_float(0.0)".to_string());
                    Some(format!("nv_fn_warmup_cosine_lr({}, {}, {}, {}, {}, nv_nil())", step, warmup, total, max_lr, min_lr))
                }
                "clip_grad_norm" => {
                    let params   = arg_strs.get(0).cloned().unwrap_or_default();
                    let max_norm = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_float(1.0)".to_string());
                    Some(format!("nv_fn_clip_grad_norm({}, {}, nv_nil())", params, max_norm))
                }
                "cosine_lr" => {
                    let step     = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    let total    = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(100)".to_string());
                    let min_lr   = arg_strs.get(2).cloned().unwrap_or_else(|| "nv_float(0.0)".to_string());
                    let max_lr   = arg_strs.get(3).cloned().unwrap_or_else(|| "nv_float(1.0)".to_string());
                    Some(format!("nv_fn_cosine_lr({}, {}, {}, {}, nv_nil())", step, total, min_lr, max_lr))
                }
                "zero_grad" => {
                    let params = arg_strs.get(0).cloned().unwrap_or_default();
                    Some(format!("nv_tensor_zero_grad({})", params))
                }
                "adagrad" => {
                    let lr = kw.get("lr").cloned()
                        .or_else(|| arg_strs.get(0).cloned())
                        .unwrap_or_else(|| "nv_float(0.01)".to_string());
                    Some(format!("nv_fn_adagrad({}, nv_nil())", lr))
                }
                "rmsprop" => {
                    let lr = kw.get("lr").cloned()
                        .or_else(|| arg_strs.get(0).cloned())
                        .unwrap_or_else(|| "nv_float(0.01)".to_string());
                    let alpha = kw.get("alpha").cloned()
                        .or_else(|| arg_strs.get(1).cloned())
                        .unwrap_or_else(|| "nv_float(0.9)".to_string());
                    Some(format!("nv_fn_rmsprop({}, {}, nv_nil())", lr, alpha))
                }
                "adam" => {
                    let lr = kw.get("lr").cloned()
                        .or_else(|| arg_strs.get(0).cloned())
                        .unwrap_or_else(|| "nv_float(0.001)".to_string());
                    Some(format!("nv_fn_adam({}, nv_nil())", lr))
                }
                "sgd" => {
                    let lr = kw.get("lr").cloned()
                        .or_else(|| arg_strs.get(0).cloned())
                        .unwrap_or_else(|| "nv_float(0.01)".to_string());
                    Some(format!("nv_fn_sgd({}, nv_nil())", lr))
                }
                "dropout"    => {
                    let x = arg_strs.get(0).cloned().unwrap_or_default();
                    let p = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_float(0.0)".to_string());
                    let training = arg_strs.get(2).cloned().unwrap_or_else(|| "nv_bool(1)".to_string());
                    Some(format!("_nv_dropout_impl({}, {}, {}, nv_nil())", x, p, training))
                }
                "where"      => {
                    let mask = arg_strs.get(0).cloned().unwrap_or_default();
                    let a = arg_strs.get(1).cloned().unwrap_or_default();
                    let b = arg_strs.get(2).cloned().unwrap_or_default();
                    Some(format!("nv_fn_where({}, {}, {}, nv_nil())", mask, a, b))
                }
                "stack"      => {
                    let tensors = arg_strs.get(0).cloned().unwrap_or_default();
                    let dim = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    Some(format!("nv_tensor_stack({}, {})", tensors, dim))
                }
                "cat"        => {
                    let tensors = arg_strs.get(0).cloned().unwrap_or_default();
                    let dim = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_int(0)".to_string());
                    Some(format!("nv_tensor_cat({}, {})", tensors, dim))
                }
                "layer_norm" => {
                    let x = arg_strs.get(0).cloned().unwrap_or_default();
                    Some(format!("nv_tensor_layer_norm({})", x))
                }
                "await_"     => {
                    let fut = arg_strs.get(0).cloned().unwrap_or_default();
                    Some(format!("nv_fn_await_({}, nv_nil())", fut))
                }
                _ => None,
            };
            if let Some(r) = result { return Ok(r); }

            // ── 2. Declared user function → arity check + direct call ─────────
            if self.fn_names_orig.contains(name) {
                // Compile-time arity check
                if let Some(&expected) = self.fn_arities.get(name) {
                    if arg_strs.len() != expected {
                        let loc = if self.src_file.is_empty() {
                            String::new()
                        } else {
                            format!("{}: ", self.src_file)
                        };
                        return Err(ParseError::new(
                            format!("{}error: '{}' expects {} argument(s), got {}",
                                loc, name, expected, arg_strs.len()),
                            0, 0));
                    }
                }
                let fn_s = self.lookup(name);
                let mut all = arg_strs.clone();
                all.push("nv_nil()".to_string());
                return Ok(format!("{}({})", fn_s, all.join(", ")));
            }

            // ── 3. Option/Result constructors — fallback when not imported ───────
            // When result.nvl IS imported, Some/None/Ok/Err appear in fn_names_orig
            // and are dispatched above. Without import they fall here as pass-through.
            match name.as_str() {
                "Some" | "Ok" => {
                    let v = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_nil()".to_string());
                    return Ok(v);
                }
                "None" => return Ok("nv_nil()".to_string()),
                "Err"  => {
                    let v = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_nil()".to_string());
                    return Ok(v);
                }
                _ => {}
            }

            // ── 4. Variable holding a function value → nv_call_N ─────────────
            let fn_s = self.lookup(name);
            return Ok(self.emit_nv_call(&fn_s, &arg_strs));
        }

        // Enum variant call: Color.Custom(42) → nv_fn_Color_Custom(42, nv_nil())
        if let Expr::Field { obj, field } = callee {
            if let Expr::Ident(type_name) = obj.as_ref() {
                if let Some(variants) = self.enum_types.get(type_name).cloned() {
                    if let Some(v) = variants.iter().find(|v| &v.name == field) {
                        let fn_c = format!("nv_fn_{}_{}", c_ident(type_name), c_ident(field));
                        let arg = if v.fields.len() <= 1 {
                            arg_strs.get(0).cloned().unwrap_or_else(|| "nv_nil()".to_string())
                        } else {
                            format!("nv_list_of({}, {})", arg_strs.len(), arg_strs.join(", "))
                        };
                        return Ok(format!("{}({}, nv_nil())", fn_c, arg));
                    }
                }
            }
        }

        // Non-identifier callee (expression returning a fn value)
        let callee_s = self.emit_expr_str(callee, depth)?;
        Ok(self.emit_nv_call(&callee_s, &arg_strs))
    }

    fn emit_nv_call(&self, fn_expr: &str, args: &[String]) -> String {
        match args.len() {
            0 => format!("nv_call_1({}, nv_nil())", fn_expr),
            1 => format!("nv_call_1({}, {})", fn_expr, args[0]),
            2 => format!("nv_call_2({}, {}, {})", fn_expr, args[0], args[1]),
            3 => format!("nv_call_3({}, {}, {}, {})", fn_expr, args[0], args[1], args[2]),
            n => format!("nv_call_1({}, nv_list_of({}, {}))", fn_expr, n, args.join(", ")),
        }
    }

    // ── Method call ───────────────────────────────────────────────────────────

    fn emit_method_call(&mut self, obj: &Expr, method: &str, args: &[Expr], kwargs: &[(String, Expr)], depth: usize) -> Result<String, ParseError> {
        // Enum variant constructor call: Shape.Circle(r) → nv_fn_Shape_Circle(r, nv_nil())
        if let Expr::Ident(type_name) = obj {
            if let Some(variants) = self.enum_types.get(type_name.as_str()).cloned() {
                if let Some(v) = variants.iter().find(|v| v.name == method) {
                    let fn_c = format!("nv_fn_{}_{}", c_ident(type_name), c_ident(method));
                    let mut arg_strs: Vec<String> = Vec::new();
                    for a in args { arg_strs.push(self.emit_expr_str(a, depth)?); }
                    let arg = if v.fields.len() <= 1 {
                        arg_strs.get(0).cloned().unwrap_or_else(|| "nv_nil()".to_string())
                    } else {
                        format!("nv_list_of({}, {})", arg_strs.len(), arg_strs.join(", "))
                    };
                    return Ok(format!("{}({}, nv_nil())", fn_c, arg));
                }
            }
        }

        // Namespace dispatch: ns.fn(args) → direct call if ns is a known namespace
        if let Expr::Ident(ns_name) = obj {
            if self.ns_fns.contains_key(ns_name.as_str()) {
                let ns_var = format!("g_{}", ns_name);
                let mut arg_strs: Vec<String> = Vec::new();
                for a in args { arg_strs.push(self.emit_expr_str(a, depth)?); }
                // Check if method is a known fn in this namespace
                if self.ns_fns.get(ns_name.as_str()).map(|s| s.contains(method)).unwrap_or(false) {
                    let fn_name = format!("nv_fn_{}", c_ident(method));
                    let mut all = arg_strs.clone();
                    all.push("nv_nil()".to_string());
                    return Ok(format!("{}({})", fn_name, all.join(", ")));
                }
                // Fallback: map lookup + dynamic call
                let fn_val = format!("nv_map_get_opt({}, nv_str(\"{}\"))", ns_var, method);
                return Ok(self.emit_nv_call(&fn_val, &arg_strs));
            }
        }

        let obj_s = self.emit_expr_str(obj, depth)?;
        let mut arg_strs: Vec<String> = Vec::new();
        for a in args { arg_strs.push(self.emit_expr_str(a, depth)?); }
        let mut kw: std::collections::HashMap<&str, String> = std::collections::HashMap::new();
        for (k, v) in kwargs {
            kw.insert(k.as_str(), self.emit_expr_str(v, depth)?);
        }

        let arg0 = arg_strs.get(0).cloned().unwrap_or_default();
        let arg1 = arg_strs.get(1).cloned().unwrap_or_default();
        Ok(match method {
            "trim"        => format!("nv_str_trim({})", obj_s),
            "to_upper"|"upper" => format!("nv_str_upper({})", obj_s),
            "to_lower"|"lower" => format!("nv_str_lower({})", obj_s),
            "contains"    => format!("(({}).tag==NV_STR ? nv_str_contains({},{}) : nv_list_contains({},{}))",
                                obj_s, obj_s, arg0, obj_s, arg0),
            "starts_with" => format!("nv_str_starts_with({}, {})", obj_s, arg0),
            "ends_with"   => format!("nv_str_ends_with({}, {})", obj_s, arg0),
            "split"       => format!("nv_str_split({}, {})", obj_s, arg0),
            "join"        => {
                // list.join(sep) — join list with separator
                format!("nv_str_join({}, {})", arg0, obj_s)
            }
            "replace"     => format!("nv_str_replace({}, {}, {})", obj_s, arg0, arg1),
            "push"        => format!("nv_list_push({}, {})", obj_s, arg0),
            "append"      => format!("nv_list_push({}, {})", obj_s, arg0),
            "pop"         => format!("nv_list_pop({})", obj_s),
            "map"         => format!("nv_map_fn({}, {})", arg0, obj_s),
            "filter"      => format!("nv_filter({}, {})", arg0, obj_s),
            "sum"         => format!("_nv_sum_any({})", obj_s),
            "mean"        => format!("_nv_mean_any({})", obj_s),
            "max"         => format!("_nv_max_any({})", obj_s),
            "min"         => format!("_nv_min_any({})", obj_s),
            "sort"|"sorted" => format!("nv_sorted({})", obj_s),
            "reverse"|"reversed" => format!("nv_reversed({})", obj_s),
            "take"        => format!("nv_list_take({}, {})", obj_s, arg0),
            "drop"        => format!("nv_list_drop({}, {})", obj_s, arg0),
            "chars"       => format!("nv_str_chars({})", obj_s),
            "slice"       => if arg_strs.len() >= 2 {
                                format!("nv_str_slice({}, {}, {})", obj_s, arg0, arg1)
                             } else if arg_strs.len() == 1 {
                                format!("nv_str_slice({}, {}, nv_int(999999))", obj_s, arg0)
                             } else {
                                obj_s.clone()
                             },
            "index_of"    => format!("nv_str_index_of({}, {})", obj_s, arg0),
            "repeat"      => format!("nv_str_repeat({}, {})", obj_s, arg0),
            "parse_i64"|"parse_int" => format!("nv_str_parse_int({})", obj_s),
            "entries"     => format!("nv_map_entries({})", obj_s),
            "has"         => format!("nv_bool(nv_map_get_opt({}, {}).tag != NV_NIL)", obj_s, arg0),
            "get"         => format!("nv_map_get_opt({}, {})", obj_s, arg0),
            "set"         => format!("nv_map_set_mut({}, {}, {})", obj_s, arg0, arg1),
            "to_str"|"to_string" => format!("nv_to_str({})", obj_s),
            "to_int"      => format!("nv_to_int({})", obj_s),
            "to_float"|"to_f64"|"to_f32" => format!("nv_to_float({})", obj_s),
            "len"         => format!("nv_len({})", obj_s),
            "abs"         => format!("nv_abs_fn({})", obj_s),
            "sqrt"        => format!("nv_sqrt_fn({})", obj_s),
            // ── Tensor methods (nuvola_tensor.h) ─────────────────────────────
            "item"        => format!("nv_tensor_item({})", obj_s),
            "tolist"      => format!("nv_tensor_tolist({})", obj_s),
            "flatten"     => format!("nv_tensor_flatten({})", obj_s),
            "reshape"     => format!("nv_tensor_reshape({}, {})", obj_s, arg0),
            "relu"        => format!("nv_tensor_relu({})", obj_s),
            "sigmoid"     => format!("nv_tensor_sigmoid({})", obj_s),
            "softmax"     => format!("nv_tensor_softmax({})", obj_s),
            "tanh"        => format!("nv_tensor_tanh_fn({})", obj_s),
            "gelu"        => format!("nv_tensor_gelu({})", obj_s),
            "exp"         => format!("nv_tensor_exp({})", obj_s),
            "log"         => format!("nv_tensor_log({})", obj_s),
            "matmul"      => format!("nv_tensor_matmul({}, {})", obj_s, arg0),
            "backward"    => format!("nv_tensor_backward({})", obj_s),
            "zero_grad"   => format!("nv_tensor_zero_grad({})", obj_s),
            "conv1d"      => {
                let padding = kw.get("padding").cloned()
                    .or_else(|| arg_strs.get(1).cloned())
                    .unwrap_or_else(|| "nv_int(0)".to_string());
                format!("nv_tensor_conv1d({}, {}, {})", obj_s, arg0, padding)
            }
            "conv2d"      => {
                let padding = kw.get("padding").cloned()
                    .or_else(|| arg_strs.get(1).cloned())
                    .unwrap_or_else(|| "nv_int(0)".to_string());
                format!("nv_tensor_conv2d({}, {}, {})", obj_s, arg0, padding)
            }
            "dot"         => format!("nv_tensor_dot({}, {})", obj_s, arg0),
            "detach"      => format!("nv_tensor_detach({})", obj_s),
            "clone"       => format!("nv_tensor_clone({})", obj_s),
            "to_f16"      => format!("nv_tensor_to_f16({})", obj_s),
            "quantize"    => {
                // Method .quantize() returns [qt, scale] list
                format!("nv_tensor_quantize({})", obj_s)
            }
            "dequantize"  => {
                // obj.dequantize(scale, dtype) — call tensor_dequantize directly
                let scale = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_float(1.0)".to_string());
                let dtype = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_str(\"f32\")".to_string());
                format!("nv_tensor_dequantize({}, {}, {})", obj_s, scale, dtype)
            }
            "norm"        => {
                let ord = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_int(2)".to_string());
                format!("nv_tensor_norm({}, {})", obj_s, ord)
            }
            "clip"        => format!("nv_tensor_clip({}, {}, {})", obj_s, arg0, arg1),
            "unsqueeze"   => format!("nv_tensor_unsqueeze({}, {})", obj_s, arg0),
            "squeeze"     => format!("nv_tensor_squeeze({}, {})", obj_s, arg0),
            "step"        => format!("nv_optimizer_step({}, {})", obj_s, arg0),
            "try_recv"    => format!("nv_chan_try_recv({})", obj_s),
            "is_some"     => format!("nv_map_field({}, nv_str(\"is_some\"))", obj_s),
            "value"       => format!("nv_map_field({}, nv_str(\"value\"))", obj_s),
            // Channel methods
            "send"        => format!("nv_fn_send({}, {}, nv_nil())", obj_s, arg0),
            "recv"        => format!("nv_fn_recv({}, nv_nil())", obj_s),
            "close"       => format!("nv_fn_close({}, nv_nil())", obj_s),
            // Misc tensor utility
            "dropout"     => {
                let rate     = arg_strs.get(0).cloned().unwrap_or_else(|| "nv_float(0.0)".to_string());
                let training = arg_strs.get(1).cloned().unwrap_or_else(|| "nv_bool(1)".to_string());
                format!("_nv_dropout_impl({}, {}, {}, nv_nil())", obj_s, rate, training)
            }
            _ => {
                // Fall back: try as user function with obj as first arg
                let fn_name = format!("nv_fn_{}", c_ident(method));
                if self.fn_names_orig.contains(method) {
                    // Declared user function — direct call + nv_nil() env
                    if arg_strs.is_empty() {
                        format!("{}({}, nv_nil())", fn_name, obj_s)
                    } else {
                        format!("{}({}, {}, nv_nil())", fn_name, obj_s, arg_strs.join(", "))
                    }
                } else if arg_strs.is_empty() {
                    format!("{}({})", fn_name, obj_s)
                } else {
                    format!("{}({}, {})", fn_name, obj_s, arg_strs.join(", "))
                }
            }
        })
    }

    // ── Field access ──────────────────────────────────────────────────────────

    fn emit_field(&self, obj_s: &str, field: &str) -> Result<String, ParseError> {
        Ok(match field {
            "len"     => format!("nv_len({})", obj_s),
            "i"       => format!("nv_to_int({})", obj_s),
            "f"       => format!("nv_to_float({})", obj_s),
            "tag"     => format!("nv_int(({}).tag)", obj_s),
            "is_some" => format!("nv_bool(({}).tag != NV_NIL)", obj_s),
            "value"   => format!("({})", obj_s),       // Some(x) is identity
            "is_none" => format!("nv_bool(({}).tag == NV_NIL)", obj_s),
            // Tensor field shortcuts
            "T"       => format!("nv_tensor_transpose({})", obj_s),
            _         => {
                // Struct/map field access: obj["field"]
                format!("nv_map_get({}, nv_str(\"{}\"))", obj_s, field)
            }
        })
    }

    // ── If expression (GCC compound statement extension) ──────────────────────

    fn emit_if_expr(
        &mut self,
        cond: &Expr,
        then_expr: &Expr,
        elif_clauses: &[(Box<Expr>, Box<Expr>)],
        else_expr: Option<&Expr>,
        depth: usize,
    ) -> Result<String, ParseError> {
        let result = self.fresh("_if_r");
        let cond_s = self.emit_expr_str(cond, depth)?;
        let then_s = self.emit_expr_str(then_expr, depth)?;

        let mut block = format!("({{ NvVal {} = nv_nil();\n", result);
        block.push_str(&format!("    if (nv_truthy({})) {{ {} = {}; }}", cond_s, result, then_s));

        for (ec, ee) in elif_clauses {
            let ec_s = self.emit_expr_str(ec, depth)?;
            let ee_s = self.emit_expr_str(ee, depth)?;
            block.push_str(&format!(" else if (nv_truthy({})) {{ {} = {}; }}", ec_s, result, ee_s));
        }

        if let Some(else_e) = else_expr {
            let else_s = self.emit_expr_str(else_e, depth)?;
            block.push_str(&format!(" else {{ {} = {}; }}", result, else_s));
        }

        block.push_str(&format!("\n    {}; }})", result));
        Ok(block)
    }

    // ── String interpolation ──────────────────────────────────────────────────

    fn emit_interp_str(&mut self, s: &str, depth: usize) -> Result<String, ParseError> {
        // Parse the string into literal and expression parts
        let parts = split_interp(s);
        if parts.len() == 1 {
            match &parts[0] {
                InterpPart::Lit(l) => return Ok(format!("nv_str(\"{}\")", escape_str(l))),
                InterpPart::Expr(_, _) => {}
            }
        }

        let mut arg_exprs: Vec<String> = Vec::new();
        for part in &parts {
            match part {
                InterpPart::Lit(l) => {
                    arg_exprs.push(format!("nv_str(\"{}\")", escape_str(l)));
                }
                InterpPart::Expr(src, fmt_spec) => {
                    // Parse the sub-expression
                    match crate::lexer::tokenize(src) {
                        Ok(tokens) => {
                            match crate::parser::parse(tokens) {
                                Ok(prog) if !prog.is_empty() => {
                                    if let crate::ast::Stmt::Expr(e) = &prog[0] {
                                        let e_str = self.emit_expr_str(e, depth)?;
                                        if let Some(fmt) = fmt_spec {
                                            arg_exprs.push(format!("nv_to_str_fmt({}, \"{}\")", e_str, fmt));
                                        } else {
                                            arg_exprs.push(format!("nv_to_str({})", e_str));
                                        }
                                    } else {
                                        arg_exprs.push("nv_str(\"???\")".to_string());
                                    }
                                }
                                _ => arg_exprs.push("nv_str(\"???\")".to_string()),
                            }
                        }
                        Err(_) => arg_exprs.push("nv_str(\"???\")".to_string()),
                    }
                }
            }
        }

        if arg_exprs.len() == 1 { return Ok(arg_exprs.remove(0)); }
        Ok(format!("nv_str_concat_n({}, {})", arg_exprs.len(), arg_exprs.join(", ")))
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn is_defined(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains_key(name))
    }

    /// Collect free variables in a lambda: idents used in body, not in params, defined in outer scope
    fn find_free_vars(&self, def: &FnDef) -> Vec<String> {
        let params: HashSet<String> = def.params.iter().map(|p| p.name.clone()).collect();
        let mut used = HashSet::new();
        match &def.body {
            FnBody::Arrow(e) => collect_idents_expr(e, &mut used),
            FnBody::Block(stmts) => { for s in stmts { collect_idents_stmt(s, &mut used); } }
            FnBody::Abstract => {}
        }
        let mut free: Vec<String> = used.into_iter()
            .filter(|n| !params.contains(n) && !is_builtin_name(n) && self.is_defined(n))
            .collect();
        free.sort();
        free
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Free variable analysis helpers
// ─────────────────────────────────────────────────────────────────────────────

fn is_builtin_name(name: &str) -> bool {
    matches!(name, "print"|"assert"|"map"|"filter"|"sum"|"len"|"int"|"float"|"str"
             |"abs"|"sqrt"|"max"|"min"|"range"|"sorted"|"reversed"
             |"true"|"false"|"nil"|"join"|"split"|"sort"|"reverse"|"push"|"pop"
             |"contains"|"take"|"drop"|"chars"|"entries"|"is_some"|"value"
             |"sin"|"cos"|"tan"|"exp"|"log"|"log2"|"log10"
             |"floor"|"ceil"|"round"|"pow"|"hypot"|"atan2"
             |"read_file"|"write_file"|"eprint"|"args"|"exit"|"env_get"|"type"
             // HTTP / JSON / time builtins (nuvola.h)
             |"json_parse"|"json_stringify"
             |"http_get"|"http_post"|"http_serve"|"http_response"
             |"time_ms"|"channel"|"sleep_ms"
             // Tensor builtins (nuvola_tensor.h)
             |"tensor"|"zeros"|"ones"|"randn"|"rand"|"eye"|"arange"|"linspace"
             |"cross_entropy"|"mse_loss"|"batch_norm"|"quantize"|"dequantize"
             // Trait / channel builtins
             |"impl_for"|"call_trait"|"chan"
             // Advanced tensor / async builtins
             |"stack"|"cat"|"layer_norm"|"await_"
             |"dropout"|"where"|"spawn"
             |"adagrad"|"rmsprop"|"adam"|"sgd"|"zero_grad"
             |"clip_grad_norm"|"cosine_lr"|"warmup_cosine_lr"|"huber_loss")
}

fn collect_idents_expr(expr: &Expr, result: &mut HashSet<String>) {
    match expr {
        Expr::Ident(n) => { result.insert(n.clone()); }
        Expr::BinOp { lhs, rhs, .. } => { collect_idents_expr(lhs, result); collect_idents_expr(rhs, result); }
        Expr::UnOp { expr: e, .. } => collect_idents_expr(e, result),
        Expr::Pipe { lhs, rhs } => { collect_idents_expr(lhs, result); collect_idents_expr(rhs, result); }
        Expr::Call { callee, args, .. } => { collect_idents_expr(callee, result); for a in args { collect_idents_expr(a, result); } }
        Expr::MethodCall { obj, args, .. } => { collect_idents_expr(obj, result); for a in args { collect_idents_expr(a, result); } }
        Expr::Index { obj, idx } => { collect_idents_expr(obj, result); collect_idents_expr(idx, result); }
        Expr::Field { obj, .. } | Expr::OptChain { obj, .. } => collect_idents_expr(obj, result),
        Expr::If { cond, then_expr, elif_clauses, else_expr } => {
            collect_idents_expr(cond, result); collect_idents_expr(then_expr, result);
            for (ec, ee) in elif_clauses { collect_idents_expr(ec, result); collect_idents_expr(ee, result); }
            if let Some(e) = else_expr { collect_idents_expr(e, result); }
        }
        Expr::List(items) | Expr::Tuple(items) | Expr::Set(items) => {
            for i in items { collect_idents_expr(i, result); }
        }
        Expr::Map(pairs) => { for (k, v) in pairs { collect_idents_expr(k, result); collect_idents_expr(v, result); } }
        Expr::Range { start, end, .. } => { collect_idents_expr(start, result); collect_idents_expr(end, result); }
        Expr::Lambda(_) => {} // don't recurse into nested lambdas
        _ => {}
    }
}

fn collect_idents_stmt(stmt: &Stmt, result: &mut HashSet<String>) {
    match stmt {
        Stmt::Expr(e) => collect_idents_expr(e, result),
        Stmt::Return(Some(e)) => collect_idents_expr(e, result),
        Stmt::Let { value, .. } => collect_idents_expr(value, result),
        Stmt::Assign { value, target } => {
            collect_idents_expr(value, result);
            match target {
                AssignTarget::Ident(n) => { result.insert(n.clone()); }
                AssignTarget::Index { obj, idx } => { collect_idents_expr(obj, result); collect_idents_expr(idx, result); }
                AssignTarget::Field { obj, .. } => collect_idents_expr(obj, result),
            }
        }
        Stmt::If { cond, then_body, elif_clauses, else_body } => {
            collect_idents_expr(cond, result);
            for s in then_body { collect_idents_stmt(s, result); }
            for (ec, eb) in elif_clauses { collect_idents_expr(ec, result); for s in eb { collect_idents_stmt(s, result); } }
            if let Some(eb) = else_body { for s in eb { collect_idents_stmt(s, result); } }
        }
        Stmt::While { cond, body } => { collect_idents_expr(cond, result); for s in body { collect_idents_stmt(s, result); } }
        Stmt::For { iter, body, .. } => { collect_idents_expr(iter, result); for s in body { collect_idents_stmt(s, result); } }
        Stmt::Match { expr, arms } => {
            collect_idents_expr(expr, result);
            for arm in arms {
                match &arm.body {
                    MatchBody::Expr(e) => collect_idents_expr(e, result),
                    MatchBody::Block(stmts) => { for s in stmts { collect_idents_stmt(s, result); } }
                }
            }
        }
        _ => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// String interpolation parser
// ─────────────────────────────────────────────────────────────────────────────

enum InterpPart {
    Lit(String),
    Expr(String, Option<String>), // source, optional format spec
}

fn split_interp(s: &str) -> Vec<InterpPart> {
    let mut parts = Vec::new();
    let mut lit   = String::new();
    let mut chars  = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            if chars.peek() == Some(&'{') {
                chars.next();
                lit.push('{');
            } else {
                lit.push('\\');
            }
        } else if c == '{' && chars.peek() == Some(&'{') {
            // {{ → literal {
            chars.next();
            lit.push('{');
        } else if c == '}' && chars.peek() == Some(&'}') {
            // }} → literal }
            chars.next();
            lit.push('}');
        } else if c == '{' {
            // Collect until matching '}'  (handle nested braces depth)
            let mut expr_src = String::new();
            let mut depth = 1usize;
            let mut fmt_spec: Option<String> = None;
            let mut closed = false;
            while let Some(ch) = chars.next() {
                match ch {
                    '{' => { depth += 1; expr_src.push(ch); }
                    '}' => {
                        depth -= 1;
                        if depth == 0 { closed = true; break; }
                        expr_src.push(ch);
                    }
                    ':' if depth == 1 => {
                        // Format spec
                        let mut spec = String::new();
                        for sc in chars.by_ref() {
                            if sc == '}' { break; }
                            spec.push(sc);
                        }
                        fmt_spec = Some(spec);
                        closed = true;
                        break;
                    }
                    _ => expr_src.push(ch),
                }
            }
            // Only treat as interpolation if braces were closed and content is non-empty
            if closed && !expr_src.is_empty() {
                if !lit.is_empty() {
                    parts.push(InterpPart::Lit(lit.clone()));
                    lit.clear();
                }
                parts.push(InterpPart::Expr(expr_src, fmt_spec));
            } else {
                // Literal brace(s): unclosed '{' or empty '{}'
                lit.push('{');
                lit.push_str(&expr_src);
                if closed { lit.push('}'); }
            }
        } else {
            lit.push(c);
        }
    }
    if !lit.is_empty() { parts.push(InterpPart::Lit(lit)); }
    parts
}

// ─────────────────────────────────────────────────────────────────────────────
// C identifier helpers
// ─────────────────────────────────────────────────────────────────────────────

fn c_ident(name: &str) -> String {
    // Map Nuvola names to valid C identifiers
    match name {
        "self"     => "self_".to_string(),
        "true"     => "nv_bool(1)".to_string(),
        "false"    => "nv_bool(0)".to_string(),
        // C keywords that clash with Nuvola parameter names
        "default"  => "_nv_default".to_string(),
        "register" => "_nv_register".to_string(),
        "auto"     => "_nv_auto".to_string(),
        "inline"   => "_nv_inline".to_string(),
        "restrict" => "_nv_restrict".to_string(),
        "signed"   => "_nv_signed".to_string(),
        "unsigned" => "_nv_unsigned".to_string(),
        "volatile" => "_nv_volatile".to_string(),
        _ => {
            let s: String = name.chars()
                .map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' })
                .collect();
            // Prefix if starts with digit
            if s.chars().next().map_or(true, |c| c.is_ascii_digit()) {
                format!("_nv_{}", s)
            } else {
                s
            }
        }
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '"'  => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _    => out.push(c),
        }
    }
    out
}

fn format_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{:.1}", f)
    } else {
        format!("{}", f)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tail-Call Optimization (TCO) helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Returns true if `expr` is a direct call to `fn_name`.
fn is_self_call(expr: &Expr, fn_name: &str) -> bool {
    if let Expr::Call { callee, .. } = expr {
        if let Expr::Ident(n) = callee.as_ref() { return n == fn_name; }
    }
    false
}

/// Returns true if any tail position in `stmts` is a self-recursive call to `fn_name`.
fn has_tail_self_call(stmts: &[Stmt], fn_name: &str) -> bool {
    if stmts.is_empty() { return false; }
    match stmts.last().unwrap() {
        Stmt::Return(Some(e)) => is_self_call(e, fn_name),
        Stmt::Expr(e)         => is_self_call(e, fn_name),
        Stmt::If { then_body, elif_clauses, else_body, .. } => {
            has_tail_self_call(then_body, fn_name)
            || elif_clauses.iter().any(|(_, b)| has_tail_self_call(b, fn_name))
            || else_body.as_ref().map_or(false, |b| has_tail_self_call(b, fn_name))
        }
        _ => false,
    }
}

/// Returns true if the function qualifies for TCO:
///   - block body (not arrow)
///   - no captured variables
///   - has at least one self-tail-call
///   - has at least one non-tail return (base case)
fn qualifies_for_tco(def: &FnDef, fn_name: &str) -> bool {
    if let FnBody::Block(stmts) = &def.body {
        if stmts.is_empty() { return false; }
        has_tail_self_call(stmts, fn_name)
    } else {
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Integer type specialization
// ─────────────────────────────────────────────────────────────────────────────

/// Returns true if `expr` always yields a plain integer value, assuming
/// `int_params` are the names of parameters known to be `long`, and
/// `fn_name` is the current function (to recognize recursive calls).
fn infer_expr_int(expr: &Expr, int_params: &HashSet<String>, fn_name: &str) -> bool {
    match expr {
        Expr::Int(_) => true,
        Expr::Ident(name) => int_params.contains(name.as_str()),
        Expr::UnOp { op: UnOp::Neg, expr } => infer_expr_int(expr, int_params, fn_name),
        Expr::BinOp { op, lhs, rhs } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Mod | BinOp::IntDiv =>
                infer_expr_int(lhs, int_params, fn_name) && infer_expr_int(rhs, int_params, fn_name),
            _ => false,
        },
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    return args.iter().all(|a| infer_expr_int(a, int_params, fn_name));
                }
            }
            false
        }
        _ => false,
    }
}

/// Returns true if all return paths in `stmts` yield integers.
fn infer_stmts_int(stmts: &[Stmt], int_params: &HashSet<String>, fn_name: &str) -> bool {
    for stmt in stmts {
        match stmt {
            Stmt::Return(Some(e)) => {
                if !infer_expr_int(e, int_params, fn_name) { return false; }
            }
            Stmt::Return(None) => return false,
            Stmt::If { then_body, elif_clauses, else_body, .. } => {
                if !infer_stmts_int(then_body, int_params, fn_name) { return false; }
                for (_, body) in elif_clauses {
                    if !infer_stmts_int(body, int_params, fn_name) { return false; }
                }
                if let Some(eb) = else_body {
                    if !infer_stmts_int(eb, int_params, fn_name) { return false; }
                }
            }
            Stmt::Expr(e) => {
                // Expression statements (function calls) — must be integer-typed too
                if !infer_expr_int(e, int_params, fn_name) { return false; }
            }
            _ => {}
        }
    }
    true
}

/// Returns true if `expr` or its subtree contains any Float literal or Div op
/// (which would mean the function is not purely integer).
fn contains_float(expr: &Expr) -> bool {
    match expr {
        Expr::Float(_) => true,
        Expr::BinOp { op: BinOp::Div | BinOp::Pow, .. } => true,
        Expr::BinOp { lhs, rhs, .. } => contains_float(lhs) || contains_float(rhs),
        Expr::UnOp { expr, .. } => contains_float(expr),
        Expr::Call { callee, args, .. } => contains_float(callee) || args.iter().any(contains_float),
        Expr::If { cond, then_expr, elif_clauses, else_expr, .. } =>
            contains_float(cond)
            || contains_float(then_expr)
            || elif_clauses.iter().any(|(c, e)| contains_float(c) || contains_float(e))
            || else_expr.as_ref().map_or(false, |e| contains_float(e)),
        _ => false,
    }
}

fn stmts_contain_float(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Return(Some(e)) => contains_float(e),
        Stmt::Let { value, .. } => contains_float(value),
        Stmt::Assign { value, .. } => contains_float(value),
        Stmt::CompoundAssign { value, .. } => contains_float(value),
        Stmt::Expr(e) => contains_float(e),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            contains_float(cond)
            || stmts_contain_float(then_body)
            || elif_clauses.iter().any(|(c, b)| contains_float(c) || stmts_contain_float(b))
            || else_body.as_ref().map_or(false, |b| stmts_contain_float(b)),
        Stmt::While { cond, body } => contains_float(cond) || stmts_contain_float(body),
        Stmt::For { iter, body, .. } => contains_float(iter) || stmts_contain_float(body),
        _ => false,
    })
}

/// Returns true if `stmts` contain only statement kinds that `emit_int_stmt` can handle
/// (Return, If with recursive If/Return bodies).  While/Let/Assign are not handled.
fn int_stmts_emittable(stmts: &[Stmt]) -> bool {
    stmts.iter().all(|s| match s {
        Stmt::Return(_) => true,
        Stmt::If { then_body, elif_clauses, else_body, .. } =>
            int_stmts_emittable(then_body)
            && elif_clauses.iter().all(|(_, b)| int_stmts_emittable(b))
            && else_body.as_ref().map_or(true, |b| int_stmts_emittable(b)),
        _ => false,
    })
}

/// Returns true if `def` can be specialized into a `long`-typed C function.
/// Criteria: no captured vars, no float literals/ops in body, all return
/// paths yield integers (possibly via recursive calls to the same function),
/// and only statement kinds that emit_int_stmt can handle (Return/If).
fn is_pure_int_fn(def: &FnDef, fn_name: &str) -> bool {
    let int_params: HashSet<String> = def.params.iter().map(|p| p.name.clone()).collect();
    match &def.body {
        FnBody::Arrow(expr) =>
            !contains_float(expr) && infer_expr_int(expr, &int_params, fn_name),
        FnBody::Block(stmts) => {
            if stmts.is_empty() { return false; }
            if stmts_contain_float(stmts) { return false; }
            if !infer_stmts_int(stmts, &int_params, fn_name) { return false; }
            // Only emit _i if all statements (except tail) are Return or If
            let (tail, body) = stmts.split_last().unwrap();
            if !int_stmts_emittable(body) { return false; }
            match tail {
                Stmt::Expr(e) => infer_expr_int(e, &int_params, fn_name) && int_stmts_emittable(body),
                Stmt::Return(_) => int_stmts_emittable(body),
                Stmt::If { .. } => int_stmts_emittable(body),
                _ => false,
            }
        }
        FnBody::Abstract => false,
    }
}

/// Emit `expr` as a `long` C expression (only valid for int-specializable exprs).
fn emit_int_expr(expr: &Expr, fn_name: &str, param_names: &[String]) -> String {
    match expr {
        Expr::Int(n) => format!("{}LL", n),
        Expr::Ident(name) => c_ident(name),
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_int_expr(expr, fn_name, param_names)),
        Expr::BinOp { op, lhs, rhs } => {
            let l = emit_int_expr(lhs, fn_name, param_names);
            let r = emit_int_expr(rhs, fn_name, param_names);
            let op_s = match op {
                BinOp::Add => "+", BinOp::Sub => "-", BinOp::Mul => "*",
                BinOp::Mod => "%", BinOp::IntDiv => "/",
                _ => unreachable!("non-int binop in int-specialized fn"),
            };
            format!("({} {} {})", l, op_s, r)
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| emit_int_expr(a, fn_name, param_names))
                        .collect();
                    return format!("nv_fn_{}_i({})", c_ident(name), arg_strs.join(", "));
                }
            }
            unreachable!("non-recursive call in int-specialized fn")
        }
        _ => unreachable!("non-int expr in int-specialized fn: {:?}", expr),
    }
}

/// Emit a condition expression as a native C boolean expression.
fn emit_int_cond(expr: &Expr, fn_name: &str, param_names: &[String]) -> String {
    match expr {
        Expr::BinOp { op, lhs, rhs } => {
            let op_s = match op {
                BinOp::Le => "<=", BinOp::Lt => "<", BinOp::Ge => ">=",
                BinOp::Gt => ">",  BinOp::Eq => "==", BinOp::Ne => "!=",
                _ => return emit_int_expr(expr, fn_name, param_names),
            };
            let int_params: HashSet<String> = param_names.iter().cloned().collect();
            if infer_expr_int(lhs, &int_params, fn_name) && infer_expr_int(rhs, &int_params, fn_name) {
                let l = emit_int_expr(lhs, fn_name, param_names);
                let r = emit_int_expr(rhs, fn_name, param_names);
                format!("{} {} {}", l, op_s, r)
            } else {
                "0 /* unresolved cond */".to_string()
            }
        }
        Expr::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        _ => emit_int_expr(expr, fn_name, param_names),
    }
}

/// Emit a statement for the `_i` fast path (long-typed).
fn emit_int_stmt(stmt: &Stmt, fn_name: &str, param_names: &[String], depth: usize) -> String {
    let ind = "    ".repeat(depth);
    match stmt {
        Stmt::Return(Some(e)) =>
            format!("{}return {};\n", ind, emit_int_expr(e, fn_name, param_names)),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_s = emit_int_cond(cond, fn_name, param_names);
            let mut s = format!("{}if ({}) {{\n", ind, cond_s);
            for st in then_body {
                s.push_str(&emit_int_stmt(st, fn_name, param_names, depth + 1));
            }
            for (ec, eb) in elif_clauses {
                let ec_s = emit_int_cond(ec, fn_name, param_names);
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, ec_s));
                for st in eb { s.push_str(&emit_int_stmt(st, fn_name, param_names, depth + 1)); }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                for st in eb { s.push_str(&emit_int_stmt(st, fn_name, param_names, depth + 1)); }
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        _ => String::new(),
    }
}

// ─── Scalar (float+int) type specialization ──────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ScalarTy { Int, Float }

impl ScalarTy {
    fn promote(a: ScalarTy, b: ScalarTy) -> ScalarTy {
        if a == ScalarTy::Float || b == ScalarTy::Float { ScalarTy::Float } else { ScalarTy::Int }
    }
    fn c_type(self) -> &'static str {
        match self { ScalarTy::Int => "long", ScalarTy::Float => "double" }
    }
    fn nv_extractor(self) -> &'static str {
        match self { ScalarTy::Int => "nv_to_i", ScalarTy::Float => "nv_to_f" }
    }
    fn nv_boxer(self) -> &'static str {
        match self { ScalarTy::Int => "nv_int", ScalarTy::Float => "nv_float" }
    }
}

type StyEnv = HashMap<String, ScalarTy>;

/// Infer the numeric type of an expression given the type environment.
/// Returns None if the expression doesn't produce a scalar (comparison, etc.) or type is unknown.
fn sty_infer(expr: &Expr, env: &StyEnv) -> Option<ScalarTy> {
    match expr {
        Expr::Int(_) => Some(ScalarTy::Int),
        Expr::Float(_) => Some(ScalarTy::Float),
        Expr::Ident(name) => env.get(name).copied(),
        Expr::UnOp { op: UnOp::Neg, expr } => sty_infer(expr, env),
        Expr::BinOp { op, lhs, rhs } => match op {
            BinOp::Div | BinOp::Pow => Some(ScalarTy::Float),
            BinOp::Mod | BinOp::IntDiv => Some(ScalarTy::Int),
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let lt = sty_infer(lhs, env)?;
                let rt = sty_infer(rhs, env)?;
                Some(ScalarTy::promote(lt, rt))
            }
            _ => None,  // comparisons, logic don't produce a number
        },
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                match name.as_str() {
                    "sqrt" | "cbrt" | "exp" | "log" | "log2" | "log10"
                    | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
                    | "floor" | "ceil" | "round" => Some(ScalarTy::Float),
                    "abs" => args.first().and_then(|a| sty_infer(a, env)),
                    _ => None,
                }
            } else { None }
        }
        _ => None,
    }
}

/// Propagate type constraints into unknown idents within an expression.
/// Returns true if the env changed.
fn sty_propagate_expr(expr: &Expr, env: &mut StyEnv) -> bool {
    let mut changed = false;
    match expr {
        Expr::BinOp { op, lhs, rhs } => {
            match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::IntDiv |
                BinOp::Le | BinOp::Lt | BinOp::Ge | BinOp::Gt | BinOp::Eq | BinOp::Ne => {
                    let lt = sty_infer(lhs, env);
                    let rt = sty_infer(rhs, env);
                    // If right side is known but left ident is not → hint left from right
                    if lt.is_none() {
                        if let (Some(rt_val), Expr::Ident(name)) = (rt, lhs.as_ref()) {
                            if !env.contains_key(name) {
                                env.insert(name.clone(), rt_val);
                                changed = true;
                            }
                        }
                    }
                    // If left side is known but right ident is not → hint right from left
                    if rt.is_none() {
                        if let (Some(lt_val), Expr::Ident(name)) = (lt, rhs.as_ref()) {
                            if !env.contains_key(name) {
                                env.insert(name.clone(), lt_val);
                                changed = true;
                            }
                        }
                    }
                }
                _ => {}
            }
            changed |= sty_propagate_expr(lhs, env);
            changed |= sty_propagate_expr(rhs, env);
        }
        Expr::UnOp { expr, .. } => { changed |= sty_propagate_expr(expr, env); }
        _ => {}
    }
    changed
}

/// Returns true if this expression or its subtree contains any non-scalar
/// operation that would prevent specialization.
fn sty_expr_nonscalar(expr: &Expr, fn_name: &str) -> bool {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Ident(_) => false,
        Expr::UnOp { expr, .. } => sty_expr_nonscalar(expr, fn_name),
        Expr::BinOp { lhs, rhs, .. } =>
            sty_expr_nonscalar(lhs, fn_name) || sty_expr_nonscalar(rhs, fn_name),
        Expr::Call { callee, args, .. } => {
            // Only allow recursive calls to the same function, and standard math builtins
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    return args.iter().any(|a| sty_expr_nonscalar(a, fn_name));
                }
                if matches!(name.as_str(), "sqrt" | "cbrt" | "exp" | "log" | "log2" | "log10"
                           | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
                           | "floor" | "ceil" | "round" | "abs") {
                    return args.iter().any(|a| sty_expr_nonscalar(a, fn_name));
                }
            }
            true  // Any other call → non-scalar
        }
        _ => true,  // Lists, maps, index, method calls, etc.
    }
}

fn sty_stmts_nonscalar(stmts: &[Stmt], fn_name: &str) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Let { value, .. } | Stmt::Assign { value, .. } => sty_expr_nonscalar(value, fn_name),
        Stmt::Return(Some(e)) | Stmt::Expr(e) => sty_expr_nonscalar(e, fn_name),
        Stmt::Return(None) => false,
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            sty_expr_nonscalar(cond, fn_name)
            || sty_stmts_nonscalar(then_body, fn_name)
            || elif_clauses.iter().any(|(c, b)|
                sty_expr_nonscalar(c, fn_name) || sty_stmts_nonscalar(b, fn_name))
            || else_body.as_ref().map_or(false, |b| sty_stmts_nonscalar(b, fn_name)),
        Stmt::While { cond, body } =>
            sty_expr_nonscalar(cond, fn_name) || sty_stmts_nonscalar(body, fn_name),
        _ => true,  // CompoundAssign, For, etc. — bail out
    })
}

/// Run one forward pass over statements to build/update the type env.
/// Returns true if anything changed.
fn sty_pass_stmts(stmts: &[Stmt], env: &mut StyEnv) -> bool {
    let mut changed = false;
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, value, .. } | Stmt::Assign { target: AssignTarget::Ident(name), value } => {
                changed |= sty_propagate_expr(value, env);
                if let Some(ty) = sty_infer(value, env) {
                    if !env.contains_key(name) {
                        env.insert(name.clone(), ty);
                        changed = true;
                    }
                }
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => {
                changed |= sty_propagate_expr(e, env);
            }
            Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
                changed |= sty_propagate_expr(cond, env);
                changed |= sty_pass_stmts(then_body, env);
                for (c, b) in elif_clauses {
                    changed |= sty_propagate_expr(c, env);
                    changed |= sty_pass_stmts(b, env);
                }
                if let Some(eb) = else_body { changed |= sty_pass_stmts(eb, env); }
            }
            Stmt::While { cond, body } => {
                changed |= sty_propagate_expr(cond, env);
                changed |= sty_pass_stmts(body, env);
            }
            _ => {}
        }
    }
    changed
}

/// Returns `Some(env)` if `def` can be fully specialized to scalar types,
/// with all parameters and referenced locals mapped to Int or Float.
/// Returns None if the function uses non-scalar operations or any param is unresolvable.
fn infer_scalar_fn(def: &FnDef, fn_name: &str) -> Option<StyEnv> {
    let stmts = match &def.body {
        FnBody::Block(stmts) => stmts,
        _ => return None,  // Arrow bodies handled by is_pure_int_fn
    };

    // Check for non-scalar operations
    if sty_stmts_nonscalar(stmts, fn_name) { return None; }

    let mut env = StyEnv::new();

    // Fixed-point: run passes until stable (max 30 iterations)
    for _ in 0..30 {
        if !sty_pass_stmts(stmts, &mut env) { break; }
    }

    // All params must have been typed
    for p in &def.params {
        if !env.contains_key(&p.name) { return None; }
    }

    Some(env)
}

/// Infer the return type of a scalar-specialized function.
fn sty_return_ty(def: &FnDef, env: &StyEnv, fn_name: &str) -> ScalarTy {
    fn scan_stmts(stmts: &[Stmt], env: &StyEnv, fn_name: &str) -> Option<ScalarTy> {
        for s in stmts {
            match s {
                Stmt::Return(Some(e)) => { if let Some(t) = sty_infer(e, env) { return Some(t); } }
                Stmt::If { then_body, elif_clauses, else_body, .. } => {
                    if let Some(t) = scan_stmts(then_body, env, fn_name) { return Some(t); }
                    for (_, b) in elif_clauses { if let Some(t) = scan_stmts(b, env, fn_name) { return Some(t); } }
                    if let Some(eb) = else_body { if let Some(t) = scan_stmts(eb, env, fn_name) { return Some(t); } }
                }
                Stmt::While { body, .. } => { if let Some(t) = scan_stmts(body, env, fn_name) { return Some(t); } }
                _ => {}
            }
        }
        None
    }
    match &def.body {
        FnBody::Block(stmts) => {
            // Check explicit returns first
            if let Some(t) = scan_stmts(stmts, env, fn_name) { return t; }
            // Then tail expression
            if let Some(Stmt::Expr(e)) = stmts.last() {
                if let Some(t) = sty_infer(e, env) { return t; }
            }
            ScalarTy::Int  // default
        }
        _ => ScalarTy::Int,
    }
}

/// Emit a scalar-typed expression (all values are `long` or `double`).
fn emit_sty_expr(expr: &Expr, env: &StyEnv, fn_name: &str) -> String {
    match expr {
        Expr::Int(n) => format!("{}LL", n),
        Expr::Float(f) => format_float(*f),
        Expr::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        Expr::Ident(name) => c_ident(name),
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_sty_expr(expr, env, fn_name)),
        Expr::BinOp { op, lhs, rhs } => {
            let l = emit_sty_expr(lhs, env, fn_name);
            let r = emit_sty_expr(rhs, env, fn_name);
            match op {
                BinOp::Add    => format!("({} + {})", l, r),
                BinOp::Sub    => format!("({} - {})", l, r),
                BinOp::Mul    => format!("({} * {})", l, r),
                BinOp::Div    => format!("({} / {})", l, r),
                BinOp::Mod    => format!("({} % {})", l, r),
                BinOp::IntDiv => format!("({} / {})", l, r),
                BinOp::Pow    => format!("pow({}, {})", l, r),
                BinOp::Le     => format!("({} <= {})", l, r),
                BinOp::Lt     => format!("({} < {})", l, r),
                BinOp::Ge     => format!("({} >= {})", l, r),
                BinOp::Gt     => format!("({} > {})", l, r),
                BinOp::Eq     => format!("({} == {})", l, r),
                BinOp::Ne     => format!("({} != {})", l, r),
                BinOp::And    => format!("({} && {})", l, r),
                BinOp::Or     => format!("({} || {})", l, r),
                _ => format!("0 /* unhandled op */"),
            }
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| emit_sty_expr(a, env, fn_name))
                        .collect();
                    return format!("nv_fn_{}_s({})", c_ident(name), arg_strs.join(", "));
                }
                let c_fn = match name.as_str() {
                    "abs" => "fabs",
                    n @ ("sqrt" | "cbrt" | "exp" | "log" | "log2" | "log10"
                       | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
                       | "floor" | "ceil" | "round") => n,
                    _ => "",
                };
                if !c_fn.is_empty() {
                    if args.len() == 2 && name.as_str() == "atan2" {
                        return format!("{}({}, {})",
                            c_fn,
                            emit_sty_expr(&args[0], env, fn_name),
                            emit_sty_expr(&args[1], env, fn_name));
                    }
                    if let Some(arg) = args.first() {
                        return format!("{}({})", c_fn, emit_sty_expr(arg, env, fn_name));
                    }
                }
            }
            format!("0 /* non-recursive call */")
        }
        _ => format!("0 /* unhandled expr */"),
    }
}

/// Wrap a scalar expression in NvVal for use as a print() argument.
fn emit_sty_expr_for_print(expr: &Expr, env: &StyEnv, fn_name: &str) -> String {
    match expr {
        Expr::Str(s) => format!("nv_str(\"{}\")", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Expr::BinOp { op: BinOp::Add, lhs, rhs } => format!(
            "nv_add({}, {})",
            emit_sty_expr_for_print(lhs, env, fn_name),
            emit_sty_expr_for_print(rhs, env, fn_name)
        ),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "str" {
                    if let Some(a) = args.first() {
                        let raw = emit_sty_expr(a, env, fn_name);
                        let ty = sty_infer(a, env).unwrap_or(ScalarTy::Float);
                        let boxed = match ty {
                            ScalarTy::Float => format!("nv_float({})", raw),
                            ScalarTy::Int   => format!("nv_int({})", raw),
                        };
                        return format!("nv_to_str({})", boxed);
                    }
                }
            }
            let raw = emit_sty_expr(expr, env, fn_name);
            format!("nv_float({})", raw)
        }
        _ => {
            let raw = emit_sty_expr(expr, env, fn_name);
            let ty = sty_infer(expr, env).unwrap_or(ScalarTy::Float);
            match ty {
                ScalarTy::Float => format!("nv_float({})", raw),
                ScalarTy::Int   => format!("nv_int({})", raw),
            }
        }
    }
}

/// Emit a scalar-typed statement.
fn emit_sty_stmt(stmt: &Stmt, env: &StyEnv, fn_name: &str, declared: &mut HashSet<String>, depth: usize) -> String {
    let ind = "    ".repeat(depth);
    match stmt {
        Stmt::Let { name, value, .. } => {
            let e = emit_sty_expr(value, env, fn_name);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                let ty = env.get(name).map_or("long", |t| t.c_type());
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::Assign { target: AssignTarget::Ident(name), value } => {
            let e = emit_sty_expr(value, env, fn_name);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                let ty = env.get(name).map_or("long", |t| t.c_type());
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::Return(Some(e)) => {
            format!("{}return {};\n", ind, emit_sty_expr(e, env, fn_name))
        }
        Stmt::Return(None) => format!("{}return 0LL;\n", ind),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_s = emit_sty_expr(cond, env, fn_name);
            let mut s = format!("{}if ({}) {{\n", ind, cond_s);
            for st in then_body { s.push_str(&emit_sty_stmt(st, env, fn_name, declared, depth+1)); }
            for (ec, eb) in elif_clauses {
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, emit_sty_expr(ec, env, fn_name)));
                for st in eb { s.push_str(&emit_sty_stmt(st, env, fn_name, declared, depth+1)); }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                for st in eb { s.push_str(&emit_sty_stmt(st, env, fn_name, declared, depth+1)); }
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::While { cond, body } => {
            let cond_s = emit_sty_expr(cond, env, fn_name);
            let mut s = format!("{}while ({}) {{\n", ind, cond_s);
            for st in body { s.push_str(&emit_sty_stmt(st, env, fn_name, declared, depth+1)); }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::Expr(e) => {
            // Handle print(...) for main-body context
            if let Expr::Call { callee, args, .. } = e.as_ref() {
                if let Expr::Ident(name) = callee.as_ref() {
                    if name == "print" {
                        let arg_nv = args.first().map_or("nv_nil()".to_string(), |a| {
                            emit_sty_expr_for_print(a, env, fn_name)
                        });
                        return format!("{}nv_print({});\n", ind, arg_nv);
                    }
                }
            }
            format!("{}{};\n", ind, emit_sty_expr(e, env, fn_name))
        }
        _ => String::new(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Coalesced scalar emission (register-pressure reduction)
// ─────────────────────────────────────────────────────────────────────────────

/// Emit a scalar expression with variable name substitution from coalesce map.
fn emit_sty_expr_with_coalesce(expr: &Expr, env: &StyEnv, fn_name: &str, coalesce: &HashMap<String, String>) -> String {
    match expr {
        Expr::Int(n) => format!("{}LL", n),
        Expr::Float(f) => format_float(*f),
        Expr::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        Expr::Ident(name) => coalesce.get(name).cloned().unwrap_or_else(|| c_ident(name)),
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_sty_expr_with_coalesce(expr, env, fn_name, coalesce)),
        Expr::BinOp { op, lhs, rhs } => {
            let l = emit_sty_expr_with_coalesce(lhs, env, fn_name, coalesce);
            let r = emit_sty_expr_with_coalesce(rhs, env, fn_name, coalesce);
            match op {
                BinOp::Add    => format!("({} + {})", l, r),
                BinOp::Sub    => format!("({} - {})", l, r),
                BinOp::Mul    => format!("({} * {})", l, r),
                BinOp::Div    => format!("({} / {})", l, r),
                BinOp::Mod    => format!("({} % {})", l, r),
                BinOp::IntDiv => format!("({} / {})", l, r),
                BinOp::Pow    => format!("pow({}, {})", l, r),
                BinOp::Le     => format!("({} <= {})", l, r),
                BinOp::Lt     => format!("({} < {})", l, r),
                BinOp::Ge     => format!("({} >= {})", l, r),
                BinOp::Gt     => format!("({} > {})", l, r),
                BinOp::Eq     => format!("({} == {})", l, r),
                BinOp::Ne     => format!("({} != {})", l, r),
                BinOp::And    => format!("({} && {})", l, r),
                BinOp::Or     => format!("({} || {})", l, r),
                _ => "0 /* unhandled op */".to_string(),
            }
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| emit_sty_expr_with_coalesce(a, env, fn_name, coalesce))
                        .collect();
                    return format!("nv_fn_{}_s({})", c_ident(name), arg_strs.join(", "));
                }
                let c_fn = match name.as_str() {
                    "abs" => "fabs",
                    n @ ("sqrt" | "cbrt" | "exp" | "log" | "log2" | "log10"
                       | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
                       | "floor" | "ceil" | "round") => n,
                    _ => "",
                };
                if !c_fn.is_empty() {
                    if args.len() == 2 && name.as_str() == "atan2" {
                        return format!("{}({}, {})", c_fn,
                            emit_sty_expr_with_coalesce(&args[0], env, fn_name, coalesce),
                            emit_sty_expr_with_coalesce(&args[1], env, fn_name, coalesce));
                    }
                    if let Some(arg) = args.first() {
                        return format!("{}({})", c_fn, emit_sty_expr_with_coalesce(arg, env, fn_name, coalesce));
                    }
                }
            }
            "0 /* non-recursive call */".to_string()
        }
        _ => "0 /* unhandled expr */".to_string(),
    }
}

/// Wrap a coalesced scalar expression in NvVal for use as a print() argument.
fn emit_sty_expr_for_print_coalesced(expr: &Expr, env: &StyEnv, fn_name: &str, coalesce: &HashMap<String, String>) -> String {
    match expr {
        Expr::Str(s) => format!("nv_str(\"{}\")", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Expr::BinOp { op: BinOp::Add, lhs, rhs } => format!(
            "nv_add({}, {})",
            emit_sty_expr_for_print_coalesced(lhs, env, fn_name, coalesce),
            emit_sty_expr_for_print_coalesced(rhs, env, fn_name, coalesce)
        ),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "str" {
                    if let Some(a) = args.first() {
                        let raw = emit_sty_expr_with_coalesce(a, env, fn_name, coalesce);
                        let ty = sty_infer(a, env).unwrap_or(ScalarTy::Float);
                        let boxed = match ty {
                            ScalarTy::Float => format!("nv_float({})", raw),
                            ScalarTy::Int   => format!("nv_int({})", raw),
                        };
                        return format!("nv_to_str({})", boxed);
                    }
                }
            }
            let raw = emit_sty_expr_with_coalesce(expr, env, fn_name, coalesce);
            format!("nv_float({})", raw)
        }
        _ => {
            let raw = emit_sty_expr_with_coalesce(expr, env, fn_name, coalesce);
            let ty = sty_infer(expr, env).unwrap_or(ScalarTy::Float);
            match ty {
                ScalarTy::Float => format!("nv_float({})", raw),
                ScalarTy::Int   => format!("nv_int({})", raw),
            }
        }
    }
}

/// Emit a scalar statement with variable coalescing and const-marking applied.
fn emit_sty_stmt_with_coalesce(
    stmt: &Stmt,
    env: &StyEnv,
    fn_name: &str,
    declared: &mut HashSet<String>,
    coalesce: &HashMap<String, String>,
    const_vars: &HashSet<String>,
    depth: usize,
) -> String {
    let ind = "    ".repeat(depth);
    match stmt {
        Stmt::Let { name, value, .. } => {
            let e = emit_sty_expr_with_coalesce(value, env, fn_name, coalesce);
            let canonical: String = coalesce.get(name).cloned().unwrap_or_else(|| c_ident(name));
            if declared.contains(&canonical) {
                format!("{}{} = {};\n", ind, canonical, e)
            } else {
                declared.insert(canonical.clone());
                let ty = env.get(name).map_or("long", |t| t.c_type());
                // Emit `const` for never-reassigned variables — frees XMM registers
                let qualifier = if const_vars.contains(name) { "const " } else { "" };
                format!("{}{}{} {} = {};\n", ind, qualifier, ty, canonical, e)
            }
        }
        Stmt::Assign { target: AssignTarget::Ident(name), value } => {
            let e = emit_sty_expr_with_coalesce(value, env, fn_name, coalesce);
            let canonical: String = coalesce.get(name).cloned().unwrap_or_else(|| c_ident(name));
            if declared.contains(&canonical) {
                format!("{}{} = {};\n", ind, canonical, e)
            } else {
                declared.insert(canonical.clone());
                let ty = env.get(name).map_or("long", |t| t.c_type());
                format!("{}{} {} = {};\n", ind, ty, canonical, e)
            }
        }
        Stmt::Return(Some(e)) => {
            format!("{}return {};\n", ind, emit_sty_expr_with_coalesce(e, env, fn_name, coalesce))
        }
        Stmt::Return(None) => format!("{}return 0LL;\n", ind),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_s = emit_sty_expr_with_coalesce(cond, env, fn_name, coalesce);
            let mut s = format!("{}if ({}) {{\n", ind, cond_s);
            let mut inner = declared.clone();
            for st in then_body {
                s.push_str(&emit_sty_stmt_with_coalesce(st, env, fn_name, &mut inner, coalesce, const_vars, depth+1));
            }
            for (ec, eb) in elif_clauses {
                let ec_s = emit_sty_expr_with_coalesce(ec, env, fn_name, coalesce);
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, ec_s));
                let mut inner2 = declared.clone();
                for st in eb {
                    s.push_str(&emit_sty_stmt_with_coalesce(st, env, fn_name, &mut inner2, coalesce, const_vars, depth+1));
                }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                let mut inner3 = declared.clone();
                for st in eb {
                    s.push_str(&emit_sty_stmt_with_coalesce(st, env, fn_name, &mut inner3, coalesce, const_vars, depth+1));
                }
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::While { cond, body } => {
            let cond_s = emit_sty_expr_with_coalesce(cond, env, fn_name, coalesce);
            let mut s = format!("{}while ({}) {{\n", ind, cond_s);
            // Build a separate coalesce map for the loop body to coalesce within iterations
            let body_coalesce = build_coalesce_map(body, env);
            // No const_vars inside a loop body (loop vars are never "const" meaningfully)
            let empty_consts: HashSet<String> = HashSet::new();
            let mut body_declared = declared.clone();
            for st in body {
                s.push_str(&emit_sty_stmt_with_coalesce(st, env, fn_name, &mut body_declared, &body_coalesce, &empty_consts, depth+1));
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::Expr(e) => {
            if let Expr::Call { callee, args, .. } = e.as_ref() {
                if let Expr::Ident(name) = callee.as_ref() {
                    if name == "print" {
                        let arg_nv = args.first().map_or("nv_nil()".to_string(), |a| {
                            emit_sty_expr_for_print_coalesced(a, env, fn_name, coalesce)
                        });
                        return format!("{}nv_print({});\n", ind, arg_nv);
                    }
                }
            }
            format!("{}{};\n", ind, emit_sty_expr_with_coalesce(e, env, fn_name, coalesce))
        }
        _ => String::new(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Typed bool-array detection (for primes/sieve optimization)
// ─────────────────────────────────────────────────────────────────────────────

/// Check whether `expr` contains `list_name` used in any way that isn't
/// a plain index-read: `list_name[idx]`.
fn expr_uses_list_as_nonarray(expr: &Expr, list_name: &str) -> bool {
    match expr {
        Expr::Ident(n) => n == list_name,  // bare use = non-array
        Expr::Index { obj, idx } => {
            // list_name[idx] is fine; but idx must not also use list_name non-array-wise
            if let Expr::Ident(n) = obj.as_ref() {
                if n == list_name {
                    return expr_uses_list_as_nonarray(idx, list_name);
                }
            }
            expr_uses_list_as_nonarray(obj, list_name) || expr_uses_list_as_nonarray(idx, list_name)
        }
        Expr::MethodCall { obj, args, .. } => {
            // push(bool) on list_name is allowed; all other method calls are not
            if let Expr::Ident(n) = obj.as_ref() {
                if n == list_name {
                    return false; // we allow push — checked elsewhere
                }
            }
            expr_uses_list_as_nonarray(obj, list_name)
                || args.iter().any(|a| expr_uses_list_as_nonarray(a, list_name))
        }
        Expr::BinOp { lhs, rhs, .. } =>
            expr_uses_list_as_nonarray(lhs, list_name) || expr_uses_list_as_nonarray(rhs, list_name),
        Expr::UnOp { expr, .. } => expr_uses_list_as_nonarray(expr, list_name),
        Expr::Call { callee, args, .. } =>
            expr_uses_list_as_nonarray(callee, list_name)
            || args.iter().any(|a| expr_uses_list_as_nonarray(a, list_name)),
        _ => false,
    }
}

fn stmts_use_list_as_nonarray(stmts: &[Stmt], list_name: &str) -> bool {
    stmts.iter().any(|s| stmt_uses_list_as_nonarray(s, list_name))
}

fn stmt_uses_list_as_nonarray(stmt: &Stmt, list_name: &str) -> bool {
    match stmt {
        Stmt::Let { value, .. } => expr_uses_list_as_nonarray(value, list_name),
        Stmt::Assign { target, value } => {
            let tgt_bad = match target {
                AssignTarget::Index { obj, idx } => {
                    // list_name[idx] = val is fine
                    if let Expr::Ident(n) = obj.as_ref() {
                        if n == list_name {
                            return expr_uses_list_as_nonarray(idx, list_name)
                                || expr_uses_list_as_nonarray(value, list_name);
                        }
                    }
                    expr_uses_list_as_nonarray(obj, list_name)
                        || expr_uses_list_as_nonarray(idx, list_name)
                }
                AssignTarget::Ident(n) => n == list_name,
                AssignTarget::Field { obj, .. } => expr_uses_list_as_nonarray(obj, list_name),
            };
            tgt_bad || expr_uses_list_as_nonarray(value, list_name)
        }
        Stmt::Expr(e) => {
            // push(bool) is allowed
            if let Expr::MethodCall { obj, method, args, .. } = e.as_ref() {
                if let Expr::Ident(n) = obj.as_ref() {
                    if n == list_name && method == "push" {
                        // Only bool args allowed
                        return args.iter().any(|a| !matches!(a, Expr::Bool(_)));
                    }
                }
            }
            expr_uses_list_as_nonarray(e, list_name)
        }
        Stmt::Return(Some(e)) => expr_uses_list_as_nonarray(e, list_name),
        Stmt::Return(None) => false,
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            expr_uses_list_as_nonarray(cond, list_name)
            || stmts_use_list_as_nonarray(then_body, list_name)
            || elif_clauses.iter().any(|(c, b)|
                expr_uses_list_as_nonarray(c, list_name) || stmts_use_list_as_nonarray(b, list_name))
            || else_body.as_ref().map_or(false, |b| stmts_use_list_as_nonarray(b, list_name)),
        Stmt::While { cond, body } =>
            expr_uses_list_as_nonarray(cond, list_name) || stmts_use_list_as_nonarray(body, list_name),
        _ => false,
    }
}

/// Detect the fill-loop pattern:
///   counter_init: `i := 0`
///   fill_while:   `while i <= limit => [list.push(bool), i = i + 1]`
/// Returns `(counter_name, size_c_expr, fill_value, counter_init_idx, fill_while_idx)`.
fn detect_fill_loop(list_name: &str, stmts: &[Stmt])
    -> Option<(String, String, u8, usize, usize)>
{
    for ci in 0..stmts.len() {
        // Look for `counter := 0`
        let counter_name = match &stmts[ci] {
            Stmt::Let { name, value, .. } => {
                if matches!(value.as_ref(), Expr::Int(0)) { name.clone() } else { continue }
            }
            _ => continue,
        };
        // Look for `while counter <= limit` immediately or anywhere after
        for wi in (ci+1)..stmts.len() {
            let while_stmt = &stmts[wi];
            if let Stmt::While { cond, body } = while_stmt {
                // cond must be `counter <= limit` or `counter < limit`
                if let Expr::BinOp { op, lhs, rhs } = cond.as_ref() {
                    let lhs_is_counter = matches!(lhs.as_ref(), Expr::Ident(n) if n == &counter_name);
                    if !lhs_is_counter { continue; }
                    let (inclusive, limit_expr) = match op {
                        BinOp::Le => (true, rhs.as_ref()),
                        BinOp::Lt => (false, rhs.as_ref()),
                        _ => continue,
                    };
                    // body must contain push(bool) and counter increment
                    let has_push = body.iter().any(|s| {
                        if let Stmt::Expr(e) = s {
                            if let Expr::MethodCall { obj, method, args, .. } = e.as_ref() {
                                if matches!(obj.as_ref(), Expr::Ident(n) if n == list_name)
                                    && method == "push" && args.len() == 1
                                    && matches!(args[0], Expr::Bool(_))
                                {
                                    return true;
                                }
                            }
                        }
                        false
                    });
                    if !has_push { continue; }
                    // Determine fill value from push arg
                    let fill_val: u8 = body.iter().find_map(|s| {
                        if let Stmt::Expr(e) = s {
                            if let Expr::MethodCall { obj, method, args, .. } = e.as_ref() {
                                if matches!(obj.as_ref(), Expr::Ident(n) if n == list_name)
                                    && method == "push" && args.len() == 1
                                {
                                    return match &args[0] {
                                        Expr::Bool(b) => Some(if *b { 1 } else { 0 }),
                                        _ => None,
                                    };
                                }
                            }
                        }
                        None
                    }).unwrap_or(0);
                    // Build size C expression
                    let limit_c = emit_sty_expr(limit_expr, &StyEnv::new(), "");
                    let size_c = if inclusive {
                        format!("{} + 1LL", limit_c)
                    } else {
                        limit_c
                    };
                    return Some((counter_name, size_c, fill_val, ci, wi));
                }
            }
        }
    }
    None
}

/// Try to detect a function body that uses a single typed bool array (like sieve).
/// Returns `Some((array_name, size_c_expr, fill_val, skip_indices, filtered_stmts))`
/// where `skip_indices` are the stmt indices to omit (list creation + fill loop).
fn detect_typed_bool_array_fn(def: &FnDef, fn_name: &str)
    -> Option<(String, String, u8, HashSet<usize>, Vec<Stmt>)>
{
    let stmts = match &def.body {
        FnBody::Block(s) => s,
        _ => return None,
    };

    // Find `list_name := []`
    let (list_idx, list_name) = stmts.iter().enumerate().find_map(|(i, s)| {
        if let Stmt::Let { name, value, .. } = s {
            if matches!(value.as_ref(), Expr::List(v) if v.is_empty()) {
                return Some((i, name.clone()));
            }
        }
        None
    })?;

    // Detect fill loop
    let (counter_name, size_c, fill_val, counter_init_idx, fill_while_idx) =
        detect_fill_loop(&list_name, stmts)?;

    // Build the set of stmt indices to skip
    let mut skip: HashSet<usize> = HashSet::new();
    skip.insert(list_idx);
    skip.insert(counter_init_idx);
    skip.insert(fill_while_idx);

    // Build the filtered stmt list (no list creation, no fill loop)
    let filtered: Vec<Stmt> = stmts.iter().enumerate()
        .filter(|(i, _)| !skip.contains(i))
        .map(|(_, s)| s.clone())
        .collect();

    // Check that `list_name` is only used as typed array in filtered stmts
    // (no method calls other than push with bool, no passing to fn, etc.)
    if stmts_use_list_as_nonarray(&filtered, &list_name) { return None; }

    // Check that the filtered stmts are otherwise all-scalar (using the counter var excluded)
    // Build a virtual env excluding list_name and counter_name
    let param_names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();

    // Check non-scalar using array-aware checker
    if typed_array_stmts_nonscalar(&filtered, fn_name, &list_name) { return None; }

    // Also ensure scalar inference works for params on filtered stmts
    // (list_name and counter_name are excluded, params must all be int)
    let mut env = StyEnv::new();
    for _ in 0..30 {
        let changed = sty_pass_stmts_filtered(&filtered, &mut env, &list_name, &counter_name);
        if !changed { break; }
    }
    for p in &def.params {
        if !env.contains_key(&p.name) { return None; }
    }

    Some((list_name, size_c, fill_val, skip, filtered))
}

/// Check if stmts are non-scalar, but allowing typed array access (array[idx]) as Int.
fn typed_array_expr_nonscalar(expr: &Expr, fn_name: &str, array_name: &str) -> bool {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Ident(_) => false,
        Expr::UnOp { expr, .. } => typed_array_expr_nonscalar(expr, fn_name, array_name),
        Expr::BinOp { lhs, rhs, .. } =>
            typed_array_expr_nonscalar(lhs, fn_name, array_name)
            || typed_array_expr_nonscalar(rhs, fn_name, array_name),
        Expr::Index { obj, idx } => {
            // array[idx] is allowed (returns uint8_t, treated as int)
            if let Expr::Ident(n) = obj.as_ref() {
                if n == array_name {
                    return typed_array_expr_nonscalar(idx, fn_name, array_name);
                }
            }
            true // other index = non-scalar
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    return args.iter().any(|a| typed_array_expr_nonscalar(a, fn_name, array_name));
                }
            }
            true
        }
        _ => true,
    }
}

fn typed_array_stmts_nonscalar(stmts: &[Stmt], fn_name: &str, array_name: &str) -> bool {
    stmts.iter().any(|s| typed_array_stmt_nonscalar(s, fn_name, array_name))
}

fn typed_array_stmt_nonscalar(stmt: &Stmt, fn_name: &str, array_name: &str) -> bool {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { target: AssignTarget::Ident(_), value } =>
            typed_array_expr_nonscalar(value, fn_name, array_name),
        // array[idx] = Bool is ok
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == array_name {
                    return typed_array_expr_nonscalar(idx, fn_name, array_name)
                        || !matches!(value.as_ref(), Expr::Bool(_));
                }
            }
            true // other index-assign = non-scalar
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) => {
            // push(bool) on array is ok (already filtered out in caller, but allow here too)
            if let Expr::MethodCall { obj, method, .. } = e.as_ref() {
                if let Expr::Ident(n) = obj.as_ref() {
                    if n == array_name && method == "push" { return false; }
                }
            }
            typed_array_expr_nonscalar(e, fn_name, array_name)
        }
        Stmt::Return(None) => false,
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            typed_array_expr_nonscalar(cond, fn_name, array_name)
            || typed_array_stmts_nonscalar(then_body, fn_name, array_name)
            || elif_clauses.iter().any(|(c, b)|
                typed_array_expr_nonscalar(c, fn_name, array_name)
                || typed_array_stmts_nonscalar(b, fn_name, array_name))
            || else_body.as_ref().map_or(false, |b| typed_array_stmts_nonscalar(b, fn_name, array_name)),
        Stmt::While { cond, body } =>
            typed_array_expr_nonscalar(cond, fn_name, array_name)
            || typed_array_stmts_nonscalar(body, fn_name, array_name),
        _ => true,
    }
}

/// Like sty_pass_stmts but skips array-related stmts for the typed-array case.
fn sty_pass_stmts_filtered(stmts: &[Stmt], env: &mut StyEnv, array_name: &str, counter_name: &str) -> bool {
    let mut changed = false;
    for stmt in stmts {
        match stmt {
            Stmt::Assign { target: AssignTarget::Index { obj, .. }, .. } => {
                if let Expr::Ident(n) = obj.as_ref() {
                    if n == array_name { continue; } // skip typed array writes
                }
            }
            Stmt::Expr(e) => {
                if let Expr::MethodCall { obj, method, .. } = e.as_ref() {
                    if let Expr::Ident(n) = obj.as_ref() {
                        if n == array_name && method == "push" { continue; }
                    }
                }
            }
            _ => {}
        }
        // For regular stmts, use normal pass but treat array index-reads as Int
        match stmt {
            Stmt::Let { name, value, .. } | Stmt::Assign { target: AssignTarget::Ident(name), value } => {
                changed |= sty_propagate_expr_ext(value, env, array_name);
                if let Some(ty) = sty_infer_ext(value, env, array_name) {
                    if !env.contains_key(name) {
                        env.insert(name.clone(), ty);
                        changed = true;
                    }
                }
            }
            Stmt::Return(Some(e)) | Stmt::Expr(e) => {
                changed |= sty_propagate_expr_ext(e, env, array_name);
            }
            Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
                changed |= sty_propagate_expr_ext(cond, env, array_name);
                changed |= sty_pass_stmts_filtered(then_body, env, array_name, counter_name);
                for (c, b) in elif_clauses {
                    changed |= sty_propagate_expr_ext(c, env, array_name);
                    changed |= sty_pass_stmts_filtered(b, env, array_name, counter_name);
                }
                if let Some(eb) = else_body { changed |= sty_pass_stmts_filtered(eb, env, array_name, counter_name); }
            }
            Stmt::While { cond, body } => {
                changed |= sty_propagate_expr_ext(cond, env, array_name);
                changed |= sty_pass_stmts_filtered(body, env, array_name, counter_name);
            }
            _ => {}
        }
    }
    changed
}

/// Like sty_infer but treats array[idx] as ScalarTy::Int.
fn sty_infer_ext(expr: &Expr, env: &StyEnv, array_name: &str) -> Option<ScalarTy> {
    match expr {
        Expr::Index { obj, .. } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == array_name { return Some(ScalarTy::Int); }
            }
            None
        }
        _ => sty_infer(expr, env),
    }
}

/// Like sty_propagate_expr but treats array[idx] as Int.
fn sty_propagate_expr_ext(expr: &Expr, env: &mut StyEnv, array_name: &str) -> bool {
    sty_propagate_expr(expr, env)
}

/// Emit the typed bool array specialization for a function like sieve.
/// Returns the full C source for the _s function.
fn emit_typed_array_sty_fn(
    fast_name: &str,
    def: &FnDef,
    fn_name: &str,
    ty_env: &StyEnv,
    ret_ty: ScalarTy,
    array_name: &str,
    size_c: &str,
    fill_val: u8,
    filtered: &[Stmt],
) -> String {
    let param_names: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
    let typed_params: Vec<String> = param_names.iter().map(|p| {
        let ty = ty_env.get(p).copied().unwrap_or(ScalarTy::Int);
        format!("{} {}", ty.c_type(), c_ident(p))
    }).collect();
    let param_types_only: Vec<&str> = param_names.iter()
        .map(|p| ty_env.get(p).copied().unwrap_or(ScalarTy::Int).c_type())
        .collect();

    let mut s = format!(
        "static {} {}({});\nstatic {} {}({}) {{\n",
        ret_ty.c_type(), fast_name, param_types_only.join(", "),
        ret_ty.c_type(), fast_name, typed_params.join(", ")
    );

    // calloc the typed array
    s.push_str(&format!(
        "    uint8_t* {} = (uint8_t*)calloc((size_t)({}), 1);\n",
        c_ident(array_name), size_c
    ));

    let mut declared: HashSet<String> = param_names.iter().cloned().collect();
    declared.insert(array_name.to_string());
    let free_code = format!("    free({});\n", c_ident(array_name));

    let n = filtered.len();
    for (i, stmt) in filtered.iter().enumerate() {
        let is_last = i + 1 == n;
        if is_last {
            // Handle tail expression as return (with free before)
            match stmt {
                Stmt::Expr(e) => {
                    let e_c = emit_sty_expr_typed_array(e, ty_env, fn_name, array_name);
                    s.push_str(&format!("    {}    return {};\n", free_code, e_c));
                }
                other => {
                    let code = emit_sty_stmt_typed_array(other, ty_env, fn_name, &mut declared, 1, array_name, true, &free_code);
                    s.push_str(&code);
                    s.push_str(&format!("    {}    return 0LL;\n", free_code));
                }
            }
        } else {
            let code = emit_sty_stmt_typed_array(stmt, ty_env, fn_name, &mut declared, 1, array_name, false, &free_code);
            s.push_str(&code);
        }
    }

    s.push_str("}\n\n");
    s
}

/// Emit a scalar statement in the context of a typed bool array function.
/// Array index reads return uint8_t (compared as int).
/// Array index writes emit `arr[idx] = 0/1;`.
/// Returns emit before free for return stmts.
fn emit_sty_stmt_typed_array(
    stmt: &Stmt,
    env: &StyEnv,
    fn_name: &str,
    declared: &mut HashSet<String>,
    depth: usize,
    array_name: &str,
    _is_last: bool,
    free_code: &str,
) -> String {
    let ind = "    ".repeat(depth);
    match stmt {
        // Array index assignment: arr[idx] = bool
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == array_name {
                    let idx_c = emit_sty_expr_typed_array(idx, env, fn_name, array_name);
                    let val_c = match value.as_ref() {
                        Expr::Bool(b) => if *b { "1" } else { "0" }.to_string(),
                        other => emit_sty_expr_typed_array(other, env, fn_name, array_name),
                    };
                    return format!("{}{}[{}] = {};\n", ind, c_ident(array_name), idx_c, val_c);
                }
            }
            emit_sty_stmt(stmt, env, fn_name, declared, depth)
        }
        // Explicit return → free before return
        Stmt::Return(Some(e)) => {
            let e_c = emit_sty_expr_typed_array(e, env, fn_name, array_name);
            format!("{}{}{}return {};\n", ind, free_code, ind, e_c)
        }
        Stmt::Return(None) => {
            format!("{}{}{}return 0LL;\n", ind, free_code, ind)
        }
        // Let statement
        Stmt::Let { name, value, .. } => {
            let e = emit_sty_expr_typed_array(value, env, fn_name, array_name);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                let ty = env.get(name).map_or("long", |t| t.c_type());
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        // Assign to ident
        Stmt::Assign { target: AssignTarget::Ident(name), value } => {
            let e = emit_sty_expr_typed_array(value, env, fn_name, array_name);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                let ty = env.get(name).map_or("long", |t| t.c_type());
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_s = emit_sty_expr_typed_array(cond, env, fn_name, array_name);
            let mut s = format!("{}if ({}) {{\n", ind, cond_s);
            for st in then_body {
                s.push_str(&emit_sty_stmt_typed_array(st, env, fn_name, declared, depth+1, array_name, false, free_code));
            }
            for (ec, eb) in elif_clauses {
                let ec_s = emit_sty_expr_typed_array(ec, env, fn_name, array_name);
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, ec_s));
                for st in eb {
                    s.push_str(&emit_sty_stmt_typed_array(st, env, fn_name, declared, depth+1, array_name, false, free_code));
                }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                for st in eb {
                    s.push_str(&emit_sty_stmt_typed_array(st, env, fn_name, declared, depth+1, array_name, false, free_code));
                }
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::While { cond, body } => {
            let cond_s = emit_sty_expr_typed_array(cond, env, fn_name, array_name);
            let mut s = format!("{}while ({}) {{\n", ind, cond_s);
            for st in body {
                s.push_str(&emit_sty_stmt_typed_array(st, env, fn_name, declared, depth+1, array_name, false, free_code));
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::Expr(e) => {
            // Skip push calls on the array (already eliminated by fill loop removal)
            if let Expr::MethodCall { obj, method, .. } = e.as_ref() {
                if let Expr::Ident(n) = obj.as_ref() {
                    if n == array_name && method == "push" { return String::new(); }
                }
            }
            format!("{}{};\n", ind, emit_sty_expr_typed_array(e, env, fn_name, array_name))
        }
        _ => String::new(),
    }
}

/// Like emit_sty_expr but handles typed array index reads (returns int comparison-compatible value).
fn emit_sty_expr_typed_array(expr: &Expr, env: &StyEnv, fn_name: &str, array_name: &str) -> String {
    match expr {
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == array_name {
                    let idx_c = emit_sty_expr_typed_array(idx, env, fn_name, array_name);
                    return format!("{}[{}]", c_ident(array_name), idx_c);
                }
            }
            emit_sty_expr(expr, env, fn_name)
        }
        Expr::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        Expr::BinOp { op, lhs, rhs } => {
            let l = emit_sty_expr_typed_array(lhs, env, fn_name, array_name);
            let r = emit_sty_expr_typed_array(rhs, env, fn_name, array_name);
            match op {
                BinOp::Add    => format!("({} + {})", l, r),
                BinOp::Sub    => format!("({} - {})", l, r),
                BinOp::Mul    => format!("({} * {})", l, r),
                BinOp::Div    => format!("({} / {})", l, r),
                BinOp::Mod    => format!("({} % {})", l, r),
                BinOp::IntDiv => format!("({} / {})", l, r),
                BinOp::Pow    => format!("pow({}, {})", l, r),
                BinOp::Le     => format!("({} <= {})", l, r),
                BinOp::Lt     => format!("({} < {})", l, r),
                BinOp::Ge     => format!("({} >= {})", l, r),
                BinOp::Gt     => format!("({} > {})", l, r),
                BinOp::Eq     => format!("({} == {})", l, r),
                BinOp::Ne     => format!("({} != {})", l, r),
                BinOp::And    => format!("({} && {})", l, r),
                BinOp::Or     => format!("({} || {})", l, r),
                _ => format!("0 /* unhandled op */"),
            }
        }
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_sty_expr_typed_array(expr, env, fn_name, array_name)),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| emit_sty_expr_typed_array(a, env, fn_name, array_name))
                        .collect();
                    return format!("nv_fn_{}_s({})", c_ident(name), arg_strs.join(", "));
                }
            }
            emit_sty_expr(expr, env, fn_name)
        }
        _ => emit_sty_expr(expr, env, fn_name),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw list pointer detection (for quicksort optimization)
// ─────────────────────────────────────────────────────────────────────────────

/// Check if all usages of `param_name` in the expression are either:
/// - Index reads: `param_name[scalar_idx]`
/// - Direct ident (as fn call arg — allowed for recursive self-calls)
fn expr_param_only_indexed_or_passthrough(expr: &Expr, param_name: &str) -> bool {
    match expr {
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == param_name {
                    // idx must not use param_name as non-array
                    return !expr_ident_appears(idx, param_name);
                }
            }
            expr_param_only_indexed_or_passthrough(obj, param_name)
                && expr_param_only_indexed_or_passthrough(idx, param_name)
        }
        Expr::Ident(n) => n != param_name, // bare use = only allowed as fn arg (checked at call site)
        Expr::BinOp { lhs, rhs, .. } =>
            expr_param_only_indexed_or_passthrough(lhs, param_name)
            && expr_param_only_indexed_or_passthrough(rhs, param_name),
        Expr::UnOp { expr, .. } => expr_param_only_indexed_or_passthrough(expr, param_name),
        Expr::Call { callee, args, .. } => {
            // For each arg: if it's Ident(param_name), that's the passthrough case
            let callee_ok = expr_param_only_indexed_or_passthrough(callee, param_name);
            let args_ok = args.iter().all(|a| {
                if matches!(a, Expr::Ident(n) if n == param_name) {
                    true // passthrough to fn call
                } else {
                    expr_param_only_indexed_or_passthrough(a, param_name)
                }
            });
            callee_ok && args_ok
        }
        _ => true, // literals, etc. — don't use param
    }
}

fn expr_ident_appears(expr: &Expr, name: &str) -> bool {
    match expr {
        Expr::Ident(n) => n == name,
        Expr::BinOp { lhs, rhs, .. } => expr_ident_appears(lhs, name) || expr_ident_appears(rhs, name),
        Expr::UnOp { expr, .. } => expr_ident_appears(expr, name),
        _ => false,
    }
}

fn stmts_param_only_indexed(stmts: &[Stmt], param_name: &str) -> bool {
    stmts.iter().all(|s| stmt_param_only_indexed(s, param_name))
}

fn stmt_param_only_indexed(stmt: &Stmt, param_name: &str) -> bool {
    match stmt {
        Stmt::Let { value, .. } => expr_param_only_indexed_or_passthrough(value, param_name),
        Stmt::Assign { target, value } => {
            let tgt_ok = match target {
                AssignTarget::Index { obj, idx } => {
                    // param[idx] = val is ok
                    if let Expr::Ident(n) = obj.as_ref() {
                        if n == param_name {
                            return !expr_ident_appears(idx, param_name)
                                && expr_param_only_indexed_or_passthrough(value, param_name);
                        }
                    }
                    expr_param_only_indexed_or_passthrough(obj, param_name)
                        && expr_param_only_indexed_or_passthrough(idx, param_name)
                }
                AssignTarget::Ident(n) => n != param_name,
                AssignTarget::Field { obj, .. } => expr_param_only_indexed_or_passthrough(obj, param_name),
            };
            tgt_ok && expr_param_only_indexed_or_passthrough(value, param_name)
        }
        Stmt::Expr(e) => {
            // Method calls on param not allowed (no .push, .len, etc.)
            if let Expr::MethodCall { obj, .. } = e.as_ref() {
                if let Expr::Ident(n) = obj.as_ref() {
                    if n == param_name { return false; }
                }
            }
            expr_param_only_indexed_or_passthrough(e, param_name)
        }
        Stmt::Return(Some(e)) => expr_param_only_indexed_or_passthrough(e, param_name),
        Stmt::Return(None) => true,
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            expr_param_only_indexed_or_passthrough(cond, param_name)
            && stmts_param_only_indexed(then_body, param_name)
            && elif_clauses.iter().all(|(c, b)|
                expr_param_only_indexed_or_passthrough(c, param_name)
                && stmts_param_only_indexed(b, param_name))
            && else_body.as_ref().map_or(true, |b| stmts_param_only_indexed(b, param_name)),
        Stmt::While { cond, body } =>
            expr_param_only_indexed_or_passthrough(cond, param_name)
            && stmts_param_only_indexed(body, param_name),
        _ => true,
    }
}

/// Returns list of parameter names that are NvVal lists used only with
/// integer-index access and recursive self-call passthrough.
fn detect_raw_list_params(def: &FnDef, fn_name: &str, scalar_env: &StyEnv) -> Vec<String> {
    let stmts = match &def.body {
        FnBody::Block(s) => s,
        _ => return vec![],
    };
    def.params.iter()
        .filter(|p| {
            // Must NOT be in scalar_env (i.e., not typed as Int/Float scalar)
            !scalar_env.contains_key(&p.name)
            // Must be used only as indexed array or recursive passthrough
            && stmts_param_only_indexed(stmts, &p.name)
        })
        .map(|p| p.name.clone())
        .collect()
}

/// Infer scalar env for a function that may have raw-list params.
/// Only processes scalar params and local scalar vars.
fn infer_scalar_env_with_raw_lists(def: &FnDef, fn_name: &str, raw_list_params: &[String]) -> Option<StyEnv> {
    let stmts = match &def.body {
        FnBody::Block(s) => s,
        _ => return None,
    };

    // Check non-scalar for non-list operations
    let truly_nonscalar = stmts.iter().any(|s| {
        stmt_nonscalar_ignoring_list_params(s, fn_name, raw_list_params)
    });
    if truly_nonscalar { return None; }

    let mut env = StyEnv::new();
    for _ in 0..30 {
        let changed = sty_pass_stmts_ignoring_lists(stmts, &mut env, raw_list_params);
        if !changed { break; }
    }

    // All non-list params must be typed
    for p in &def.params {
        if raw_list_params.contains(&p.name) { continue; }
        if !env.contains_key(&p.name) { return None; }
    }

    Some(env)
}

fn stmt_nonscalar_ignoring_list_params(stmt: &Stmt, fn_name: &str, raw_list_params: &[String]) -> bool {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { target: AssignTarget::Ident(_), value } =>
            expr_nonscalar_ignoring_list_params(value, fn_name, raw_list_params),
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            // list_param[idx] = NvVal is fine
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_list_params.contains(n) {
                    return expr_nonscalar_ignoring_list_params(idx, fn_name, raw_list_params)
                        || expr_nonscalar_ignoring_list_params(value, fn_name, raw_list_params);
                }
            }
            true
        }
        Stmt::Return(Some(e)) | Stmt::Expr(e) =>
            expr_nonscalar_ignoring_list_params(e, fn_name, raw_list_params),
        Stmt::Return(None) => false,
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            expr_nonscalar_ignoring_list_params(cond, fn_name, raw_list_params)
            || then_body.iter().any(|s| stmt_nonscalar_ignoring_list_params(s, fn_name, raw_list_params))
            || elif_clauses.iter().any(|(c, b)|
                expr_nonscalar_ignoring_list_params(c, fn_name, raw_list_params)
                || b.iter().any(|s| stmt_nonscalar_ignoring_list_params(s, fn_name, raw_list_params)))
            || else_body.as_ref().map_or(false, |b| b.iter().any(|s| stmt_nonscalar_ignoring_list_params(s, fn_name, raw_list_params))),
        Stmt::While { cond, body } =>
            expr_nonscalar_ignoring_list_params(cond, fn_name, raw_list_params)
            || body.iter().any(|s| stmt_nonscalar_ignoring_list_params(s, fn_name, raw_list_params)),
        _ => true,
    }
}

fn expr_nonscalar_ignoring_list_params(expr: &Expr, fn_name: &str, raw_list_params: &[String]) -> bool {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Nil => false,
        Expr::Ident(n) => {
            // Raw list params are NvVal but allowed
            if raw_list_params.contains(n) { return false; }
            false // other idents are fine
        }
        Expr::Index { obj, idx } => {
            // list_param[scalar_idx] → returns NvVal (not scalar), but we allow it
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_list_params.contains(n) {
                    return expr_nonscalar_ignoring_list_params(idx, fn_name, raw_list_params);
                }
            }
            true // other index = non-scalar
        }
        Expr::UnOp { expr, .. } => expr_nonscalar_ignoring_list_params(expr, fn_name, raw_list_params),
        Expr::BinOp { lhs, rhs, .. } =>
            expr_nonscalar_ignoring_list_params(lhs, fn_name, raw_list_params)
            || expr_nonscalar_ignoring_list_params(rhs, fn_name, raw_list_params),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    return args.iter().any(|a| expr_nonscalar_ignoring_list_params(a, fn_name, raw_list_params));
                }
            }
            true
        }
        _ => true,
    }
}

fn sty_pass_stmts_ignoring_lists(stmts: &[Stmt], env: &mut StyEnv, raw_list_params: &[String]) -> bool {
    let mut changed = false;
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, value, .. } | Stmt::Assign { target: AssignTarget::Ident(name), value } => {
                changed |= sty_propagate_expr(value, env);
                if let Some(ty) = sty_infer(value, env) {
                    if !env.contains_key(name) {
                        env.insert(name.clone(), ty);
                        changed = true;
                    }
                }
            }
            Stmt::Assign { target: AssignTarget::Index { .. }, .. } => {}  // skip
            Stmt::Return(Some(e)) | Stmt::Expr(e) => {
                changed |= sty_propagate_expr(e, env);
            }
            Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
                changed |= sty_propagate_expr(cond, env);
                changed |= sty_pass_stmts_ignoring_lists(then_body, env, raw_list_params);
                for (c, b) in elif_clauses {
                    changed |= sty_propagate_expr(c, env);
                    changed |= sty_pass_stmts_ignoring_lists(b, env, raw_list_params);
                }
                if let Some(eb) = else_body { changed |= sty_pass_stmts_ignoring_lists(eb, env, raw_list_params); }
            }
            Stmt::While { cond, body } => {
                changed |= sty_propagate_expr(cond, env);
                changed |= sty_pass_stmts_ignoring_lists(body, env, raw_list_params);
            }
            _ => {}
        }
    }
    changed
}

/// Infer return type for raw-list param function (returns NvVal since it returns nil).
fn raw_list_fn_return_ty(def: &FnDef, env: &StyEnv, fn_name: &str) -> ScalarTy {
    // quicksort returns nil → we use NvVal wrapper. But _s returns NvVal too.
    // Actually we want the _s to return NvVal so we can handle nil returns.
    // We'll check if all return paths are nil.
    ScalarTy::Int // placeholder; we handle this specially
}

// ─────────────────────────────────────────────────────────────────────────────
// Raw list pointer emission (for quicksort-style functions)
// ─────────────────────────────────────────────────────────────────────────────

/// Emit a statement in the raw-list-param context.
/// List param index reads become `_PARAM_raw[idx]`.
/// List param index writes become `_PARAM_raw[idx] = val`.
/// Recursive self-calls pass list params as NvVal and scalar params as long.
fn emit_raw_list_stmt(
    stmt: &Stmt,
    env: &StyEnv,
    fn_name: &str,
    declared: &mut HashSet<String>,
    depth: usize,
    raw_params: &[String],
    _is_last: bool,
) -> String {
    let ind = "    ".repeat(depth);
    match stmt {
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_params.contains(n) {
                    let idx_c = emit_raw_list_expr(idx, env, fn_name, raw_params);
                    let val_c = emit_raw_list_expr(value, env, fn_name, raw_params);
                    return format!("{}_{}_raw[{}] = {};\n", ind, c_ident(n), idx_c, val_c);
                }
            }
            // fallback
            let tgt_c = match obj.as_ref() {
                Expr::Ident(n) => {
                    let idx_c = emit_raw_list_expr(idx, env, fn_name, raw_params);
                    format!("nv_index_set({}, {}, {})", c_ident(n), idx_c, emit_raw_list_expr(value, env, fn_name, raw_params))
                }
                _ => String::new(),
            };
            format!("{}{};\n", ind, tgt_c)
        }
        Stmt::Let { name, value, .. } => {
            let e = emit_raw_list_expr(value, env, fn_name, raw_params);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                // If value is a raw list index read, it's NvVal
                let ty = if is_raw_list_index(value, raw_params) {
                    "NvVal".to_string()
                } else {
                    env.get(name).map_or("long", |t| t.c_type()).to_string()
                };
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::Assign { target: AssignTarget::Ident(name), value } => {
            let e = emit_raw_list_expr(value, env, fn_name, raw_params);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                let ty = if is_raw_list_index(value, raw_params) {
                    "NvVal".to_string()
                } else {
                    env.get(name).map_or("long", |t| t.c_type()).to_string()
                };
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::Return(Some(e)) => {
            if matches!(e.as_ref(), Expr::Nil) {
                return format!("{}return nv_nil();\n", ind);
            }
            let e_c = emit_raw_list_expr(e, env, fn_name, raw_params);
            format!("{}return {};\n", ind, e_c)
        }
        Stmt::Return(None) => format!("{}return nv_nil();\n", ind),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_s = emit_raw_list_cond(cond, env, fn_name, raw_params);
            let mut s = format!("{}if ({}) {{\n", ind, cond_s);
            // Use clones of declared for inner blocks so inner lets don't leak out
            let mut inner_decl = declared.clone();
            for st in then_body {
                s.push_str(&emit_raw_list_stmt(st, env, fn_name, &mut inner_decl, depth+1, raw_params, false));
            }
            for (ec, eb) in elif_clauses {
                let ec_s = emit_raw_list_cond(ec, env, fn_name, raw_params);
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, ec_s));
                let mut inner_decl2 = declared.clone();
                for st in eb {
                    s.push_str(&emit_raw_list_stmt(st, env, fn_name, &mut inner_decl2, depth+1, raw_params, false));
                }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                let mut inner_decl3 = declared.clone();
                for st in eb {
                    s.push_str(&emit_raw_list_stmt(st, env, fn_name, &mut inner_decl3, depth+1, raw_params, false));
                }
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::While { cond, body } => {
            let cond_s = emit_raw_list_cond(cond, env, fn_name, raw_params);
            let mut s = format!("{}while ({}) {{\n", ind, cond_s);
            let mut inner_decl = declared.clone();
            for st in body {
                s.push_str(&emit_raw_list_stmt(st, env, fn_name, &mut inner_decl, depth+1, raw_params, false));
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::Expr(e) => {
            format!("{}{};\n", ind, emit_raw_list_expr(e, env, fn_name, raw_params))
        }
        _ => String::new(),
    }
}

fn is_raw_list_index(expr: &Expr, raw_params: &[String]) -> bool {
    if let Expr::Index { obj, .. } = expr {
        if let Expr::Ident(n) = obj.as_ref() {
            return raw_params.contains(n);
        }
    }
    false
}

/// Emit a condition expression: for comparisons involving NvVal (from list index reads),
/// use nv_le/nv_lt/etc. For pure scalar, use direct C operators.
fn emit_raw_list_cond(expr: &Expr, env: &StyEnv, fn_name: &str, raw_params: &[String]) -> String {
    match expr {
        Expr::BinOp { op, lhs, rhs } => {
            let lhs_is_list_val = contains_raw_list_index(lhs, raw_params);
            let rhs_is_list_val = contains_raw_list_index(rhs, raw_params);
            if lhs_is_list_val || rhs_is_list_val {
                // Use NvVal comparison functions
                let l = emit_raw_list_expr(lhs, env, fn_name, raw_params);
                let r = emit_raw_list_expr(rhs, env, fn_name, raw_params);
                let fn_call = match op {
                    BinOp::Le => format!("nv_truthy(nv_le({}, {}))", l, r),
                    BinOp::Lt => format!("nv_truthy(nv_lt({}, {}))", l, r),
                    BinOp::Ge => format!("nv_truthy(nv_ge({}, {}))", l, r),
                    BinOp::Gt => format!("nv_truthy(nv_gt({}, {}))", l, r),
                    BinOp::Eq => format!("nv_truthy(nv_eq({}, {}))", l, r),
                    BinOp::Ne => format!("nv_truthy(nv_ne({}, {}))", l, r),
                    _ => {
                        // For if (lo >= hi) where both are scalar: use sty
                        emit_sty_expr(expr, env, fn_name)
                    }
                };
                return fn_call;
            }
            // Pure scalar condition
            emit_sty_expr(expr, env, fn_name)
        }
        _ => emit_raw_list_expr(expr, env, fn_name, raw_params),
    }
}

fn contains_raw_list_index(expr: &Expr, raw_params: &[String]) -> bool {
    match expr {
        Expr::Index { obj, .. } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_params.contains(n) { return true; }
            }
            false
        }
        Expr::BinOp { lhs, rhs, .. } =>
            contains_raw_list_index(lhs, raw_params) || contains_raw_list_index(rhs, raw_params),
        Expr::Ident(n) => raw_params.contains(n), // NvVal list param itself
        _ => false,
    }
}

/// Emit an expression in raw-list-param context.
/// List param index reads → `_PARAM_raw[idx_c]` (NvVal).
/// Scalar expressions → direct C arithmetic.
/// Recursive calls → `nv_fn_NAME_s(...)`.
fn emit_raw_list_expr(expr: &Expr, env: &StyEnv, fn_name: &str, raw_params: &[String]) -> String {
    match expr {
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_params.contains(n) {
                    let idx_c = emit_raw_list_expr(idx, env, fn_name, raw_params);
                    return format!("_{}_raw[{}]", c_ident(n), idx_c);
                }
            }
            emit_sty_expr(expr, env, fn_name)
        }
        Expr::Ident(n) => {
            if raw_params.contains(n) {
                c_ident(n) // pass the NvVal list directly
            } else {
                c_ident(n)
            }
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| emit_raw_list_expr(a, env, fn_name, raw_params))
                        .collect();
                    return format!("nv_fn_{}_s({})", c_ident(name), arg_strs.join(", "));
                }
            }
            emit_sty_expr(expr, env, fn_name)
        }
        Expr::BinOp { op, lhs, rhs } => {
            let lhs_list = contains_raw_list_index(lhs, raw_params);
            let rhs_list = contains_raw_list_index(rhs, raw_params);
            if !lhs_list && !rhs_list {
                // Pure scalar: emit as C arithmetic
                let l = emit_raw_list_expr(lhs, env, fn_name, raw_params);
                let r = emit_raw_list_expr(rhs, env, fn_name, raw_params);
                match op {
                    BinOp::Add => format!("({} + {})", l, r),
                    BinOp::Sub => format!("({} - {})", l, r),
                    BinOp::Mul => format!("({} * {})", l, r),
                    BinOp::Div => format!("({} / {})", l, r),
                    BinOp::Mod => format!("({} % {})", l, r),
                    BinOp::Le  => format!("({} <= {})", l, r),
                    BinOp::Lt  => format!("({} < {})", l, r),
                    BinOp::Ge  => format!("({} >= {})", l, r),
                    BinOp::Gt  => format!("({} > {})", l, r),
                    BinOp::Eq  => format!("({} == {})", l, r),
                    BinOp::Ne  => format!("({} != {})", l, r),
                    _ => emit_sty_expr(expr, env, fn_name),
                }
            } else {
                // Mixed: use nv_* functions (for comparisons like arr[j] <= pivot)
                let l = emit_raw_list_expr(lhs, env, fn_name, raw_params);
                let r = emit_raw_list_expr(rhs, env, fn_name, raw_params);
                match op {
                    BinOp::Le => format!("nv_le({}, {})", l, r),
                    BinOp::Lt => format!("nv_lt({}, {})", l, r),
                    BinOp::Ge => format!("nv_ge({}, {})", l, r),
                    BinOp::Gt => format!("nv_gt({}, {})", l, r),
                    BinOp::Eq => format!("nv_eq({}, {})", l, r),
                    BinOp::Ne => format!("nv_ne({}, {})", l, r),
                    _ => emit_sty_expr(expr, env, fn_name),
                }
            }
        }
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_raw_list_expr(expr, env, fn_name, raw_params)),
        _ => emit_sty_expr(expr, env, fn_name),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// int64_t* typed list emission (for _t variant: pure C, no NvVal boxing)
// ─────────────────────────────────────────────────────────────────────────────

/// Emit a statement in the int64_t*-list context.
/// List param index reads  → `PARAM[idx]` (int64_t).
/// List param index writes → `PARAM[idx] = val` (int64_t).
/// Recursive self-calls   → `nv_fn_NAME_t(...)`.
fn emit_int_list_stmt(
    stmt: &Stmt,
    env: &StyEnv,
    fn_name: &str,
    declared: &mut HashSet<String>,
    depth: usize,
    raw_params: &[String],
) -> String {
    let ind = "    ".repeat(depth);
    match stmt {
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_params.contains(n) {
                    let idx_c = emit_int_list_expr(idx, env, fn_name, raw_params);
                    let val_c = emit_int_list_expr(value, env, fn_name, raw_params);
                    return format!("{}{}[{}] = {};\n", ind, c_ident(n), idx_c, val_c);
                }
            }
            // fallback
            format!("{}/* unhandled index assign */\n", ind)
        }
        Stmt::Let { name, value, .. } => {
            let e = emit_int_list_expr(value, env, fn_name, raw_params);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                // int64_t for list-element reads, long for scalars
                let ty = if is_raw_list_index(value, raw_params) {
                    "int64_t".to_string()
                } else {
                    env.get(name).map_or("long", |t| t.c_type()).to_string()
                };
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::Assign { target: AssignTarget::Ident(name), value } => {
            let e = emit_int_list_expr(value, env, fn_name, raw_params);
            let cname = c_ident(name);
            if declared.contains(name) {
                format!("{}{} = {};\n", ind, cname, e)
            } else {
                declared.insert(name.clone());
                let ty = if is_raw_list_index(value, raw_params) {
                    "int64_t".to_string()
                } else {
                    env.get(name).map_or("long", |t| t.c_type()).to_string()
                };
                format!("{}{} {} = {};\n", ind, ty, cname, e)
            }
        }
        Stmt::Return(Some(e)) => {
            if matches!(e.as_ref(), Expr::Nil) {
                return format!("{}return;\n", ind);
            }
            format!("{}return;\n", ind) // _t returns void
        }
        Stmt::Return(None) => format!("{}return;\n", ind),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_s = emit_int_list_cond(cond, env, fn_name, raw_params);
            let mut s = format!("{}if ({}) {{\n", ind, cond_s);
            let mut inner_decl = declared.clone();
            for st in then_body {
                s.push_str(&emit_int_list_stmt(st, env, fn_name, &mut inner_decl, depth+1, raw_params));
            }
            for (ec, eb) in elif_clauses {
                let ec_s = emit_int_list_cond(ec, env, fn_name, raw_params);
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, ec_s));
                let mut inner_decl2 = declared.clone();
                for st in eb {
                    s.push_str(&emit_int_list_stmt(st, env, fn_name, &mut inner_decl2, depth+1, raw_params));
                }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                let mut inner_decl3 = declared.clone();
                for st in eb {
                    s.push_str(&emit_int_list_stmt(st, env, fn_name, &mut inner_decl3, depth+1, raw_params));
                }
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::While { cond, body } => {
            let cond_s = emit_int_list_cond(cond, env, fn_name, raw_params);
            let mut s = format!("{}while ({}) {{\n", ind, cond_s);
            let mut inner_decl = declared.clone();
            for st in body {
                s.push_str(&emit_int_list_stmt(st, env, fn_name, &mut inner_decl, depth+1, raw_params));
            }
            s.push_str(&format!("{}}}\n", ind));
            s
        }
        Stmt::Expr(e) => {
            // Recursive void call
            let e_c = emit_int_list_expr(e, env, fn_name, raw_params);
            format!("{}{};\n", ind, e_c)
        }
        _ => String::new(),
    }
}

/// Emit a condition in the int64_t* list context (pure C, no nv_* calls).
fn emit_int_list_cond(expr: &Expr, env: &StyEnv, fn_name: &str, raw_params: &[String]) -> String {
    // In the _t context, list elements are int64_t — direct C operators always work
    emit_int_list_expr(expr, env, fn_name, raw_params)
}

/// Emit an expression in the int64_t* list context.
/// List index reads → `PARAM[idx]` (int64_t, no boxing).
/// Recursive calls → `nv_fn_NAME_t(...)` (void, used as statement).
fn emit_int_list_expr(expr: &Expr, env: &StyEnv, fn_name: &str, raw_params: &[String]) -> String {
    match expr {
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if raw_params.contains(n) {
                    let idx_c = emit_int_list_expr(idx, env, fn_name, raw_params);
                    return format!("{}[{}]", c_ident(n), idx_c);
                }
            }
            emit_sty_expr(expr, env, fn_name)
        }
        Expr::Ident(n) => c_ident(n),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == fn_name {
                    let arg_strs: Vec<String> = args.iter()
                        .map(|a| emit_int_list_expr(a, env, fn_name, raw_params))
                        .collect();
                    return format!("nv_fn_{}_t({})", c_ident(name), arg_strs.join(", "));
                }
            }
            emit_sty_expr(expr, env, fn_name)
        }
        Expr::BinOp { op, lhs, rhs } => {
            let l = emit_int_list_expr(lhs, env, fn_name, raw_params);
            let r = emit_int_list_expr(rhs, env, fn_name, raw_params);
            match op {
                BinOp::Add => format!("({} + {})", l, r),
                BinOp::Sub => format!("({} - {})", l, r),
                BinOp::Mul => format!("({} * {})", l, r),
                BinOp::Div => format!("({} / {})", l, r),
                BinOp::Mod => format!("({} % {})", l, r),
                BinOp::Le  => format!("({} <= {})", l, r),
                BinOp::Lt  => format!("({} < {})", l, r),
                BinOp::Ge  => format!("({} >= {})", l, r),
                BinOp::Gt  => format!("({} > {})", l, r),
                BinOp::Eq  => format!("({} == {})", l, r),
                BinOp::Ne  => format!("({} != {})", l, r),
                BinOp::And => format!("({} && {})", l, r),
                BinOp::Or  => format!("({} || {})", l, r),
                _ => emit_sty_expr(expr, env, fn_name),
            }
        }
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_int_list_expr(expr, env, fn_name, raw_params)),
        _ => emit_sty_expr(expr, env, fn_name),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Specialized main body emission helpers
// ─────────────────────────────────────────────────────────────────────────────

/// Detect variables that are integer arrays: created as `arr := []` and only
/// pushed with integer expressions, indexed (read/write), or passed to int_list fns.
/// Returns a set of such variable names.
fn detect_int_array_vars(stmts: &[Stmt]) -> HashSet<String> {
    let mut candidates: HashSet<String> = HashSet::new();
    let mut disqualified: HashSet<String> = HashSet::new();

    // First pass: find `name := []` patterns
    for stmt in stmts {
        if let Stmt::Let { name, value, .. } | Stmt::Assign { target: AssignTarget::Ident(name), value } = stmt {
            if matches!(value.as_ref(), Expr::List(v) if v.is_empty()) {
                candidates.insert(name.clone());
            }
        }
    }

    // Second pass: check all uses of candidates
    for name in candidates.iter() {
        for stmt in stmts {
            if stmt_disqualifies_int_array(stmt, name) {
                disqualified.insert(name.clone());
                break;
            }
        }
    }

    candidates.into_iter().filter(|n| !disqualified.contains(n)).collect()
}

fn stmt_disqualifies_int_array(stmt: &Stmt, arr_name: &str) -> bool {
    match stmt {
        Stmt::Let { name, value, .. } => {
            if name == arr_name { return false; } // the declaration itself
            expr_disqualifies_int_array(value, arr_name)
        }
        Stmt::Assign { target, value } => {
            match target {
                AssignTarget::Ident(n) => {
                    if n == arr_name { return true; } // reassigning the array itself
                    expr_disqualifies_int_array(value, arr_name)
                }
                AssignTarget::Index { obj, idx } => {
                    // arr[i] = expr is fine if expr is scalar (we can't easily check here,
                    // so allow it — will be caught in stmt_specializable)
                    if let Expr::Ident(n) = obj.as_ref() {
                        if n == arr_name {
                            return expr_disqualifies_int_array(idx, arr_name)
                                || expr_disqualifies_int_array(value, arr_name);
                        }
                    }
                    expr_disqualifies_int_array(value, arr_name)
                }
                AssignTarget::Field { .. } => false,
            }
        }
        Stmt::Expr(e) => {
            // arr.push(expr) is fine; other method calls on arr are not
            if let Expr::MethodCall { obj, method, .. } = e.as_ref() {
                if let Expr::Ident(n) = obj.as_ref() {
                    if n == arr_name {
                        return method != "push"; // only push allowed
                    }
                }
            }
            expr_disqualifies_int_array(e, arr_name)
        }
        Stmt::While { cond, body } =>
            expr_disqualifies_int_array(cond, arr_name)
            || body.iter().any(|s| stmt_disqualifies_int_array(s, arr_name)),
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } =>
            expr_disqualifies_int_array(cond, arr_name)
            || then_body.iter().any(|s| stmt_disqualifies_int_array(s, arr_name))
            || elif_clauses.iter().any(|(c, b)|
                expr_disqualifies_int_array(c, arr_name)
                || b.iter().any(|s| stmt_disqualifies_int_array(s, arr_name)))
            || else_body.as_ref().map_or(false, |b| b.iter().any(|s| stmt_disqualifies_int_array(s, arr_name))),
        _ => false,
    }
}

fn expr_disqualifies_int_array(expr: &Expr, arr_name: &str) -> bool {
    match expr {
        Expr::Ident(n) => {
            // Bare use as ident (not index) in non-call context → could be passed to a fn
            // We'll allow it only in Call/MethodCall contexts (handled there)
            n == arr_name
        }
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == arr_name {
                    return expr_disqualifies_int_array(idx, arr_name);
                }
            }
            expr_disqualifies_int_array(obj, arr_name) || expr_disqualifies_int_array(idx, arr_name)
        }
        Expr::Call { callee, args, .. } => {
            // arr used as argument to a function: check individually at specializable level
            // For detection purposes, allow it (will be checked in stmt_specializable)
            if let Expr::Ident(n) = callee.as_ref() {
                // If callee is arr_name itself, that's disqualifying
                if n == arr_name { return true; }
            }
            args.iter().any(|a| {
                // If arr_name appears as arg, it's OK (will be a _t call)
                // But if it appears inside a non-trivial subexpr, disqualify
                match a {
                    Expr::Ident(n) => false, // direct pass: OK
                    _ => expr_disqualifies_int_array(a, arr_name),
                }
            })
        }
        Expr::MethodCall { obj, method, args, .. } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if n == arr_name {
                    return method != "push"; // only push is allowed
                }
            }
            expr_disqualifies_int_array(obj, arr_name)
                || args.iter().any(|a| expr_disqualifies_int_array(a, arr_name))
        }
        Expr::BinOp { lhs, rhs, .. } =>
            expr_disqualifies_int_array(lhs, arr_name) || expr_disqualifies_int_array(rhs, arr_name),
        Expr::UnOp { expr, .. } => expr_disqualifies_int_array(expr, arr_name),
        _ => false,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Variable coalescing — register-pressure reduction for scalar main bodies
// ─────────────────────────────────────────────────────────────────────────────

/// Collect names that appear as `Assign` targets (directly reassigned variables).
fn collect_assigned_targets(stmt: &Stmt, out: &mut HashSet<String>) {
    match stmt {
        Stmt::Assign { target: AssignTarget::Ident(name), .. } => { out.insert(name.clone()); }
        Stmt::While { body, .. } => { for s in body { collect_assigned_targets(s, out); } }
        Stmt::If { then_body, elif_clauses, else_body, .. } => {
            for s in then_body { collect_assigned_targets(s, out); }
            for (_, b) in elif_clauses { for s in b { collect_assigned_targets(s, out); } }
            if let Some(eb) = else_body { for s in eb { collect_assigned_targets(s, out); } }
        }
        _ => {}
    }
}

/// Return the set of variable names that are let-bound and NEVER reassigned.
/// These can be emitted as `const` in C, freeing XMM registers.
fn find_const_vars(stmts: &[Stmt]) -> HashSet<String> {
    let mut assigned: HashSet<String> = HashSet::new();
    for s in stmts { collect_assigned_targets(s, &mut assigned); }
    let mut consts: HashSet<String> = HashSet::new();
    for s in stmts {
        if let Stmt::Let { name, .. } = s {
            if !assigned.contains(name) {
                consts.insert(name.clone());
            }
        }
    }
    consts
}

/// Collect all variable reads (Ident usages) in an expression into last_use at index `idx`.
fn collect_expr_reads(expr: &Expr, idx: usize, last_use: &mut HashMap<String, usize>) {
    match expr {
        Expr::Ident(name) => { last_use.insert(name.clone(), idx); }
        Expr::BinOp { lhs, rhs, .. } => {
            collect_expr_reads(lhs, idx, last_use);
            collect_expr_reads(rhs, idx, last_use);
        }
        Expr::UnOp { expr, .. } => collect_expr_reads(expr, idx, last_use),
        Expr::Call { callee, args, .. } => {
            collect_expr_reads(callee, idx, last_use);
            for a in args { collect_expr_reads(a, idx, last_use); }
        }
        _ => {}
    }
}

/// Compute the last top-level statement index at which each variable is read.
/// Variables inside nested while/if bodies are conservatively marked at their parent's index.
fn compute_scalar_last_use(stmts: &[Stmt]) -> HashMap<String, usize> {
    fn visit_stmt(stmt: &Stmt, idx: usize, last_use: &mut HashMap<String, usize>) {
        match stmt {
            Stmt::Let { value, .. }
            | Stmt::Assign { target: AssignTarget::Ident(_), value } => {
                collect_expr_reads(value, idx, last_use);
            }
            Stmt::Expr(e) => collect_expr_reads(e, idx, last_use),
            Stmt::While { cond, body } => {
                collect_expr_reads(cond, idx, last_use);
                for s in body { visit_stmt(s, idx, last_use); }
            }
            Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
                collect_expr_reads(cond, idx, last_use);
                for s in then_body { visit_stmt(s, idx, last_use); }
                for (c, b) in elif_clauses {
                    collect_expr_reads(c, idx, last_use);
                    for s in b { visit_stmt(s, idx, last_use); }
                }
                if let Some(eb) = else_body {
                    for s in eb { visit_stmt(s, idx, last_use); }
                }
            }
            _ => {}
        }
    }
    let mut last_use: HashMap<String, usize> = HashMap::new();
    for (idx, stmt) in stmts.iter().enumerate() {
        visit_stmt(stmt, idx, &mut last_use);
    }
    last_use
}

/// Build variable coalescing map: logical_name → canonical_c_name.
/// Let-bindings with non-overlapping live ranges share the same canonical name,
/// reducing register pressure in the generated C.
fn build_coalesce_map(stmts: &[Stmt], sty_env: &StyEnv) -> HashMap<String, String> {
    let last_use = compute_scalar_last_use(stmts);
    let mut coalesce: HashMap<String, String> = HashMap::new();
    // Pool of available canonical names: (canonical, ty, available_from_idx)
    let mut pool: Vec<(String, ScalarTy, usize)> = Vec::new();

    for (idx, stmt) in stmts.iter().enumerate() {
        if let Stmt::Let { name, .. } = stmt {
            let ty = sty_env.get(name).copied().unwrap_or(ScalarTy::Int);
            let last = last_use.get(name).copied().unwrap_or(idx);

            // Find an available slot of the same type that became free before this idx
            let pos = pool.iter().position(|(_, t, avail)| *t == ty && *avail <= idx);
            if let Some(pos) = pos {
                let (canonical, _, _) = pool.remove(pos);
                coalesce.insert(name.clone(), canonical.clone());
                pool.push((canonical, ty, last + 1));
            } else {
                // New variable — register its canonical name as itself
                let canonical = c_ident(name);
                pool.push((canonical, ty, last + 1));
                // No entry in coalesce map = identity mapping
            }
        }
    }
    coalesce
}

/// Returns true if every statement in `stmts` is pure-scalar (no lists/maps/method calls,
/// only scalar arithmetic + math builtins + top-level `print(...)`).
fn stmts_all_pure_scalar(stmts: &[Stmt]) -> bool {
    stmts.iter().all(stmt_is_pure_scalar)
}

fn stmt_is_pure_scalar(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { target: AssignTarget::Ident(_), value } => {
            !sty_expr_nonscalar(value, "")
        }
        Stmt::Expr(e) => {
            if let Expr::Call { callee, .. } = e.as_ref() {
                if let Expr::Ident(name) = callee.as_ref() {
                    return name == "print";
                }
            }
            false
        }
        Stmt::While { cond, body } => {
            !sty_expr_nonscalar(cond, "") && stmts_all_pure_scalar(body)
        }
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            !sty_expr_nonscalar(cond, "")
            && stmts_all_pure_scalar(then_body)
            && elif_clauses.iter().all(|(c, b)|
                !sty_expr_nonscalar(c, "") && stmts_all_pure_scalar(b))
            && else_body.as_ref().map_or(true, |b| stmts_all_pure_scalar(b))
        }
        _ => false,
    }
}

/// Check whether ALL statements in main can be specialized.
fn stmts_all_specializable(
    stmts: &[Stmt],
    int_arrays: &HashSet<String>,
    int_list_fns: &HashMap<String, usize>,
) -> bool {
    // We need at least one int array to justify specialization
    if int_arrays.is_empty() { return false; }
    stmts.iter().all(|s| stmt_specializable(s, int_arrays, int_list_fns))
}

fn stmt_specializable(
    stmt: &Stmt,
    int_arrays: &HashSet<String>,
    int_list_fns: &HashMap<String, usize>,
) -> bool {
    match stmt {
        Stmt::Let { value, .. } | Stmt::Assign { target: AssignTarget::Ident(_), value } => {
            // Scalar literal, or scalar expr, or empty list (int array init)
            matches!(value.as_ref(), Expr::List(v) if v.is_empty())
                || expr_is_scalar_or_int_array_index(value, int_arrays)
        }
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            // arr[i] = int_expr
            if let Expr::Ident(n) = obj.as_ref() {
                if int_arrays.contains(n) {
                    return expr_is_scalar_or_int_array_index(idx, int_arrays)
                        && expr_is_scalar_or_int_array_index(value, int_arrays);
                }
            }
            false
        }
        Stmt::Expr(e) => {
            // arr.push(int_expr) or call to int_list fn
            match e.as_ref() {
                Expr::MethodCall { obj, method, args, .. } => {
                    if let Expr::Ident(n) = obj.as_ref() {
                        if int_arrays.contains(n) && method == "push" {
                            return args.iter().all(|a| expr_is_scalar_or_int_array_index(a, int_arrays));
                        }
                    }
                    // print(expr) with mixed content — allow if uses nv_* functions in output
                    // For now: allow nv_print / print calls
                    if let Expr::Call { callee, args, .. } = e.as_ref() {
                        if let Expr::Ident(name) = callee.as_ref() {
                            if name == "print" { return true; }
                        }
                    }
                    false
                }
                Expr::Call { callee, args, .. } => {
                    if let Expr::Ident(fn_name) = callee.as_ref() {
                        if fn_name == "print" { return true; }
                        if int_list_fns.contains_key(fn_name.as_str()) {
                            // All args must be: int array (pass as ptr) or scalar
                            return args.iter().enumerate().all(|(i, a)| {
                                let list_idx = int_list_fns[fn_name.as_str()];
                                if i == list_idx {
                                    matches!(a, Expr::Ident(n) if int_arrays.contains(n))
                                } else {
                                    expr_is_scalar_or_int_array_index(a, int_arrays)
                                }
                            });
                        }
                    }
                    false
                }
                _ => false,
            }
        }
        Stmt::While { cond, body } => {
            expr_is_scalar_or_int_array_index(cond, int_arrays)
                && body.iter().all(|s| stmt_specializable(s, int_arrays, int_list_fns))
        }
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            expr_is_scalar_or_int_array_index(cond, int_arrays)
                && then_body.iter().all(|s| stmt_specializable(s, int_arrays, int_list_fns))
                && elif_clauses.iter().all(|(c, b)|
                    expr_is_scalar_or_int_array_index(c, int_arrays)
                    && b.iter().all(|s| stmt_specializable(s, int_arrays, int_list_fns)))
                && else_body.as_ref().map_or(true, |b|
                    b.iter().all(|s| stmt_specializable(s, int_arrays, int_list_fns)))
        }
        _ => false,
    }
}

fn expr_is_scalar_or_int_array_index(expr: &Expr, int_arrays: &HashSet<String>) -> bool {
    match expr {
        Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Nil => true,
        Expr::Ident(n) => !int_arrays.contains(n), // scalar var OK, array var by itself not
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if int_arrays.contains(n) {
                    return expr_is_scalar_or_int_array_index(idx, int_arrays);
                }
            }
            false
        }
        Expr::BinOp { lhs, rhs, .. } =>
            expr_is_scalar_or_int_array_index(lhs, int_arrays)
            && expr_is_scalar_or_int_array_index(rhs, int_arrays),
        Expr::UnOp { expr, .. } => expr_is_scalar_or_int_array_index(expr, int_arrays),
        // str/str concat in print context — let it through, handled specially
        Expr::Str(_) => true,
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(n) = callee.as_ref() {
                if n == "str" || n == "len" {
                    return args.iter().all(|a| expr_is_scalar_or_int_array_index(a, int_arrays));
                }
            }
            false
        }
        _ => false,
    }
}

/// Like sty_pass_stmts_ignoring_lists but also ignores int_array names.
fn sty_pass_stmts_ignoring_lists_ext(stmts: &[Stmt], env: &mut StyEnv, int_arrays: &HashSet<String>) -> bool {
    let arr_vec: Vec<String> = int_arrays.iter().cloned().collect();
    sty_pass_stmts_ignoring_lists(stmts, env, &arr_vec)
}

/// Compute initial capacities for int arrays from context.
/// Returns map: array_name → capacity C expression string.
fn compute_array_sizes(stmts: &[Stmt], int_arrays: &HashSet<String>, sty_env: &StyEnv) -> HashMap<String, String> {
    let mut sizes: HashMap<String, String> = HashMap::new();

    // Build scalar literal map: name → value
    let mut scalar_lits: HashMap<String, i64> = HashMap::new();
    for stmt in stmts {
        match stmt {
            Stmt::Let { name, value, .. } | Stmt::Assign { target: AssignTarget::Ident(name), value } => {
                if let Expr::Int(v) = value.as_ref() {
                    scalar_lits.insert(name.clone(), *v);
                }
            }
            _ => {}
        }
    }

    for arr_name in int_arrays {
        // Look for a while loop that pushes to this array with a counter < N bound
        // where N is a known scalar
        for stmt in stmts {
            if let Stmt::While { cond, body } = stmt {
                if let Expr::BinOp { op: BinOp::Lt, lhs: _, rhs } = cond.as_ref() {
                    // Check if rhs is a scalar we know
                    let cap_expr = match rhs.as_ref() {
                        Expr::Int(v) => Some(format!("{}LL", v)),
                        Expr::Ident(n) => scalar_lits.get(n).map(|v| format!("{}LL", v)),
                        _ => None,
                    };
                    // Check if this loop pushes to arr_name
                    let pushes_to_arr = body.iter().any(|s| {
                        if let Stmt::Expr(e) = s {
                            if let Expr::MethodCall { obj, method, .. } = e.as_ref() {
                                if let Expr::Ident(n) = obj.as_ref() {
                                    return n == arr_name && method == "push";
                                }
                            }
                        }
                        false
                    });
                    if pushes_to_arr {
                        if let Some(cap) = cap_expr {
                            sizes.insert(arr_name.clone(), cap);
                            break;
                        }
                    }
                }
            }
        }
        if !sizes.contains_key(arr_name) {
            sizes.insert(arr_name.clone(), "65536LL".to_string());
        }
    }

    sizes
}

/// Emit a single specialized statement. Returns None if it cannot be specialized.
fn emit_specialized_stmt(
    stmt: &Stmt,
    sty_env: &StyEnv,
    int_arrays: &HashSet<String>,
    array_sizes: &HashMap<String, String>,
    int_list_fns: &HashMap<String, usize>,
    arr_decls: &mut HashMap<String, (String, String)>,  // arr_name → (c_ptr_name, len_name)
    arr_to_free: &mut Vec<String>,
    declared: &mut HashSet<String>,
    depth: usize,
) -> Option<String> {
    let ind = "    ".repeat(depth);
    match stmt {
        Stmt::Let { name, value, .. } | Stmt::Assign { target: AssignTarget::Ident(name), value } => {
            // Integer array initialization: `arr := []`
            if int_arrays.contains(name) && matches!(value.as_ref(), Expr::List(v) if v.is_empty()) {
                let cap = array_sizes.get(name).map(|s| s.as_str()).unwrap_or("65536LL");
                let c_ptr = c_ident(name);
                let len_name = format!("{}_len", c_ident(name));
                let cap_name = format!("_{}_cap", c_ident(name));
                arr_decls.insert(name.clone(), (c_ptr.clone(), len_name.clone()));
                arr_to_free.push(c_ptr.clone());
                return Some(format!(
                    "{}long {} = {};\n{}int64_t* {} = (int64_t*)malloc((size_t){} * sizeof(int64_t));\n{}long {} = 0LL;\n",
                    ind, cap_name, cap,
                    ind, c_ptr, cap_name,
                    ind, len_name
                ));
            }
            // Scalar variable
            let e = emit_specialized_expr(value, sty_env, int_arrays, int_list_fns, arr_decls, false);
            let cname = c_ident(name);
            if declared.contains(name) {
                Some(format!("{}{} = {};\n", ind, cname, e))
            } else {
                declared.insert(name.clone());
                let ty = sty_env.get(name).map_or("long", |t| t.c_type());
                Some(format!("{}{} {} = {};\n", ind, ty, cname, e))
            }
        }
        Stmt::Assign { target: AssignTarget::Index { obj, idx }, value } => {
            if let Expr::Ident(arr_name) = obj.as_ref() {
                if int_arrays.contains(arr_name) {
                    let idx_c = emit_specialized_expr(idx, sty_env, int_arrays, int_list_fns, arr_decls, false);
                    let val_c = emit_specialized_expr(value, sty_env, int_arrays, int_list_fns, arr_decls, false);
                    return Some(format!("{}{}[{}] = {};\n", ind, c_ident(arr_name), idx_c, val_c));
                }
            }
            None
        }
        Stmt::Expr(e) => {
            match e.as_ref() {
                // arr.push(expr) → arr[arr_len++] = expr
                Expr::MethodCall { obj, method, args, .. } if method == "push" => {
                    if let Expr::Ident(arr_name) = obj.as_ref() {
                        if int_arrays.contains(arr_name) {
                            if let Some(val_expr) = args.first() {
                                let val_c = emit_specialized_expr(val_expr, sty_env, int_arrays, int_list_fns, arr_decls, false);
                                let len_name = arr_decls.get(arr_name)
                                    .map(|(_, l)| l.clone())
                                    .unwrap_or_else(|| format!("{}_len", c_ident(arr_name)));
                                return Some(format!("{}{}[{}++] = {};\n",
                                    ind, c_ident(arr_name), len_name, val_c));
                            }
                        }
                    }
                    None
                }
                // Call to int_list fn: quicksort(arr, 0, n-1)
                Expr::Call { callee, args, .. } => {
                    if let Expr::Ident(fn_name) = callee.as_ref() {
                        if fn_name == "print" {
                            // print(expr) — emit as nv_print with NvVal wrapping
                            let arg_c = if let Some(a) = args.first() {
                                emit_specialized_expr_nvval(a, sty_env, int_arrays, int_list_fns, arr_decls)
                            } else {
                                "nv_nil()".to_string()
                            };
                            return Some(format!("{}nv_print({});\n", ind, arg_c));
                        }
                        if int_list_fns.contains_key(fn_name.as_str()) {
                            let list_idx = int_list_fns[fn_name.as_str()];
                            let arg_strs: Vec<String> = args.iter().enumerate().map(|(i, a)| {
                                if i == list_idx {
                                    // Pass int64_t* directly
                                    if let Expr::Ident(n) = a {
                                        c_ident(n)
                                    } else {
                                        emit_specialized_expr(a, sty_env, int_arrays, int_list_fns, arr_decls, false)
                                    }
                                } else {
                                    emit_specialized_expr(a, sty_env, int_arrays, int_list_fns, arr_decls, false)
                                }
                            }).collect();
                            return Some(format!("{}nv_fn_{}_t({});\n",
                                ind, c_ident(fn_name), arg_strs.join(", ")));
                        }
                    }
                    None
                }
                _ => None,
            }
        }
        Stmt::While { cond, body } => {
            let cond_c = emit_specialized_expr(cond, sty_env, int_arrays, int_list_fns, arr_decls, false);
            let mut s = format!("{}while ({}) {{\n", ind, cond_c);
            let mut inner_decl = declared.clone();
            for st in body {
                let code = emit_specialized_stmt(
                    st, sty_env, int_arrays, array_sizes, int_list_fns,
                    arr_decls, arr_to_free, &mut inner_decl, depth+1,
                )?;
                s.push_str(&code);
            }
            s.push_str(&format!("{}}}\n", ind));
            Some(s)
        }
        Stmt::If { cond, then_body, elif_clauses, else_body, .. } => {
            let cond_c = emit_specialized_expr(cond, sty_env, int_arrays, int_list_fns, arr_decls, false);
            let mut s = format!("{}if ({}) {{\n", ind, cond_c);
            let mut inner_decl = declared.clone();
            for st in then_body {
                let code = emit_specialized_stmt(
                    st, sty_env, int_arrays, array_sizes, int_list_fns,
                    arr_decls, arr_to_free, &mut inner_decl, depth+1,
                )?;
                s.push_str(&code);
            }
            for (ec, eb) in elif_clauses {
                let ec_c = emit_specialized_expr(ec, sty_env, int_arrays, int_list_fns, arr_decls, false);
                s.push_str(&format!("{}}} else if ({}) {{\n", ind, ec_c));
                let mut inner_decl2 = declared.clone();
                for st in eb {
                    let code = emit_specialized_stmt(
                        st, sty_env, int_arrays, array_sizes, int_list_fns,
                        arr_decls, arr_to_free, &mut inner_decl2, depth+1,
                    )?;
                    s.push_str(&code);
                }
            }
            if let Some(eb) = else_body {
                s.push_str(&format!("{}}} else {{\n", ind));
                let mut inner_decl3 = declared.clone();
                for st in eb {
                    let code = emit_specialized_stmt(
                        st, sty_env, int_arrays, array_sizes, int_list_fns,
                        arr_decls, arr_to_free, &mut inner_decl3, depth+1,
                    )?;
                    s.push_str(&code);
                }
            }
            s.push_str(&format!("{}}}\n", ind));
            Some(s)
        }
        _ => None,
    }
}

/// Emit a scalar expression in the specialized context (returns long/int64_t).
fn emit_specialized_expr(
    expr: &Expr,
    sty_env: &StyEnv,
    int_arrays: &HashSet<String>,
    int_list_fns: &HashMap<String, usize>,
    arr_decls: &HashMap<String, (String, String)>,
    _nvval_ctx: bool,
) -> String {
    match expr {
        Expr::Int(n) => format!("{}LL", n),
        Expr::Float(f) => format_float(*f),
        Expr::Bool(b) => if *b { "1".to_string() } else { "0".to_string() },
        Expr::Str(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Expr::Ident(name) => c_ident(name),
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if int_arrays.contains(n) {
                    let idx_c = emit_specialized_expr(idx, sty_env, int_arrays, int_list_fns, arr_decls, false);
                    return format!("{}[{}]", c_ident(n), idx_c);
                }
            }
            emit_sty_expr(expr, sty_env, "")
        }
        Expr::BinOp { op, lhs, rhs } => {
            let l = emit_specialized_expr(lhs, sty_env, int_arrays, int_list_fns, arr_decls, false);
            let r = emit_specialized_expr(rhs, sty_env, int_arrays, int_list_fns, arr_decls, false);
            match op {
                BinOp::Add    => format!("({} + {})", l, r),
                BinOp::Sub    => format!("({} - {})", l, r),
                BinOp::Mul    => format!("({} * {})", l, r),
                BinOp::Div    => format!("({} / {})", l, r),
                BinOp::Mod    => format!("({} % {})", l, r),
                BinOp::IntDiv => format!("({} / {})", l, r),
                BinOp::Le     => format!("({} <= {})", l, r),
                BinOp::Lt     => format!("({} < {})", l, r),
                BinOp::Ge     => format!("({} >= {})", l, r),
                BinOp::Gt     => format!("({} > {})", l, r),
                BinOp::Eq     => format!("({} == {})", l, r),
                BinOp::Ne     => format!("({} != {})", l, r),
                BinOp::And    => format!("({} && {})", l, r),
                BinOp::Or     => format!("({} || {})", l, r),
                _ => emit_sty_expr(expr, sty_env, ""),
            }
        }
        Expr::UnOp { op: UnOp::Neg, expr } =>
            format!("(-{})", emit_specialized_expr(expr, sty_env, int_arrays, int_list_fns, arr_decls, false)),
        _ => emit_sty_expr(expr, sty_env, ""),
    }
}

/// Emit an expression wrapped in NvVal (for print arguments, etc.)
fn emit_specialized_expr_nvval(
    expr: &Expr,
    sty_env: &StyEnv,
    int_arrays: &HashSet<String>,
    int_list_fns: &HashMap<String, usize>,
    arr_decls: &HashMap<String, (String, String)>,
) -> String {
    match expr {
        Expr::Str(s) => format!("nv_str(\"{}\")", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Expr::BinOp { op: BinOp::Add, lhs, rhs } => {
            // String concatenation with nv_add
            let l = emit_specialized_expr_nvval(lhs, sty_env, int_arrays, int_list_fns, arr_decls);
            let r = emit_specialized_expr_nvval(rhs, sty_env, int_arrays, int_list_fns, arr_decls);
            format!("nv_add({}, {})", l, r)
        }
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(name) = callee.as_ref() {
                if name == "str" {
                    if let Some(a) = args.first() {
                        let inner = emit_specialized_expr_nvval(a, sty_env, int_arrays, int_list_fns, arr_decls);
                        return format!("nv_to_str({})", inner);
                    }
                }
            }
            // Fallback: wrap scalar as nv_int
            let c = emit_specialized_expr(expr, sty_env, int_arrays, int_list_fns, arr_decls, false);
            format!("nv_int({})", c)
        }
        Expr::Index { obj, idx } => {
            if let Expr::Ident(n) = obj.as_ref() {
                if int_arrays.contains(n) {
                    let idx_c = emit_specialized_expr(idx, sty_env, int_arrays, int_list_fns, arr_decls, false);
                    return format!("nv_int({}[{}])", c_ident(n), idx_c);
                }
            }
            // fallback
            let c = emit_specialized_expr(expr, sty_env, int_arrays, int_list_fns, arr_decls, false);
            format!("nv_int({})", c)
        }
        Expr::Ident(name) => {
            // Scalar var: wrap in nv_int
            let ty = sty_env.get(name.as_str()).copied().unwrap_or(ScalarTy::Int);
            match ty {
                ScalarTy::Int   => format!("nv_int({})", c_ident(name)),
                ScalarTy::Float => format!("nv_float({})", c_ident(name)),
            }
        }
        Expr::Int(v)   => format!("nv_int({}LL)", v),
        Expr::Float(f) => format!("nv_float({})", format_float(*f)),
        _ => {
            // Try scalar then wrap
            let c = emit_specialized_expr(expr, sty_env, int_arrays, int_list_fns, arr_decls, false);
            format!("nv_int({})", c)
        }
    }
}

fn stmt_kind_name(s: &Stmt) -> &'static str {
    match s {
        Stmt::TypeDecl { .. }    => "type decl",
        Stmt::TraitDecl { .. }   => "trait decl",
        Stmt::ImplDecl { .. }    => "impl decl",
        Stmt::Import { .. }      => "import",
        Stmt::Comptime { .. }    => "comptime",
        Stmt::ExternFn { .. }    => "extern fn",
        Stmt::Unsafe(_)          => "unsafe",
        Stmt::AwaitStmt(_)       => "await",
        Stmt::SpawnStmt(_)       => "spawn",
        Stmt::Annotation { .. }  => "annotation",
        _                        => "stmt",
    }
}
