/// nuvc — Nuvola Stage-0 Bootstrap Compiler
///
/// Milestone M1: lexer complete.
/// Milestone M2: parser complete.
/// Milestone M3: codegen (AST → C → binary via clang).
///
/// Usage:
///   nuvc --lex   <file>          Dump all tokens with span info
///   nuvc --count <file>          Print token count (quick sanity check)
///   nuvc --parse <file>          Parse and dump the AST
///   nuvc         <file> -o <out> Compile to native binary

mod ast;
mod check;
mod codegen;
mod error;
mod fmt;
mod lexer;
mod parser;
mod token;

use std::{env, fs, path::Path, process};
use token::TokenKind;

// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("nuvc: error: no input file");
        eprintln!("Usage:");
        eprintln!("  nuvc --lex   <file>     dump tokens");
        eprintln!("  nuvc --count <file>     print token count");
        eprintln!("  nuvc         <file>     compile  (not yet implemented)");
        process::exit(1);
    }

    match args[1].as_str() {
        "--lex"   => cmd_lex(&args),
        "--count" => cmd_count(&args),
        "--parse" => cmd_parse(&args),
        "--check" => cmd_check(&args),
        "--fmt"   => cmd_fmt(&args),
        "--help" | "-h" => {
            print_help();
        }
        flag if flag.starts_with('-') => {
            eprintln!("nuvc: unknown flag `{}`", flag);
            process::exit(1);
        }
        _ => cmd_compile(&args),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// --lex  :  dump all tokens
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_lex(args: &[String]) {
    if args.len() < 3 {
        eprintln!("nuvc: --lex requires a file argument");
        process::exit(1);
    }
    let src    = read_file(&args[2]);
    let tokens = lex_or_die(&src, &args[2]);

    // Column header
    println!("{:<14} {:<10} {}", "SPAN", "KIND", "VALUE");
    println!("{}", "-".repeat(60));

    for tok in &tokens {
        // Pretty-print each token: span on the left, kind on the right.
        let span_str  = format!("{}:{}", tok.span.line, tok.span.col);
        let kind_str  = match &tok.kind {
            TokenKind::Int(n)    => format!("Int       {}", n),
            TokenKind::Float(v)  => format!("Float     {}", v),
            TokenKind::Str(s)    => format!("Str       {:?}", truncate(s, 40)),
            TokenKind::Bool(b)   => format!("Bool      {}", b),
            TokenKind::Nil       => "Nil".to_string(),
            TokenKind::Ident(s)  => format!("Ident     {}", s),
            TokenKind::Kw(k)     => format!("Kw        {}", k),
            TokenKind::Annot(s)  => format!("Annot     {}", s),
            TokenKind::Op(op)    => format!("Op        {}", op),
            TokenKind::Newline   => "Newline".to_string(),
            TokenKind::Indent    => "Indent   >>>".to_string(),
            TokenKind::Dedent    => "Dedent   <<<".to_string(),
            TokenKind::Eof       => "Eof".to_string(),
        };
        println!("{:<14} {}", span_str, kind_str);
    }

    println!();
    println!("{} tokens", tokens.len());
}

// ─────────────────────────────────────────────────────────────────────────────
// --count : quick token count
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_count(args: &[String]) {
    if args.len() < 3 {
        eprintln!("nuvc: --count requires a file argument");
        process::exit(1);
    }
    let src    = read_file(&args[2]);
    let tokens = lex_or_die(&src, &args[2]);

    // Breakdown by category
    let mut ints    = 0usize;
    let mut floats  = 0usize;
    let mut strs    = 0usize;
    let mut bools   = 0usize;
    let mut nils    = 0usize;
    let mut idents  = 0usize;
    let mut kws     = 0usize;
    let mut annots  = 0usize;
    let mut ops     = 0usize;
    let mut nls     = 0usize;
    let mut indents = 0usize;
    let mut dedents = 0usize;

    for tok in &tokens {
        match &tok.kind {
            TokenKind::Int(_)    => ints    += 1,
            TokenKind::Float(_)  => floats  += 1,
            TokenKind::Str(_)    => strs    += 1,
            TokenKind::Bool(_)   => bools   += 1,
            TokenKind::Nil       => nils    += 1,
            TokenKind::Ident(_)  => idents  += 1,
            TokenKind::Kw(_)     => kws     += 1,
            TokenKind::Annot(_)  => annots  += 1,
            TokenKind::Op(_)     => ops     += 1,
            TokenKind::Newline   => nls     += 1,
            TokenKind::Indent    => indents += 1,
            TokenKind::Dedent    => dedents += 1,
            TokenKind::Eof       => {}
        }
    }

    println!("File:      {}", args[2]);
    println!("Tokens:    {}", tokens.len());
    println!("  Idents   {}", idents);
    println!("  Keywords {}", kws);
    println!("  Ops      {}", ops);
    println!("  Ints     {}", ints);
    println!("  Floats   {}", floats);
    println!("  Strings  {}", strs);
    println!("  Bools    {}", bools);
    println!("  Nils     {}", nils);
    println!("  Annots   {}", annots);
    println!("  Newlines {}", nls);
    println!("  Indents  {}", indents);
    println!("  Dedents  {}", dedents);
}

// ─────────────────────────────────────────────────────────────────────────────
// --parse  :  parse and dump AST
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_parse(args: &[String]) {
    if args.len() < 3 {
        eprintln!("nuvc: --parse requires a file argument");
        process::exit(1);
    }
    let src    = read_file(&args[2]);
    let tokens = lex_or_die(&src, &args[2]);
    match parser::parse(tokens) {
        Ok(program) => {
            println!("{:#?}", program);
            eprintln!("nuvc: parsed {} — {} top-level statements", args[2], program.len());
        }
        Err(e) => {
            eprintln!("nuvc: {}: parse error: {}", args[2], e);
            process::exit(1);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// --check : static analysis (arity, duplicate defs)
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_check(args: &[String]) {
    if args.len() < 3 {
        eprintln!("nuvc: --check requires a file argument");
        process::exit(1);
    }
    let src    = read_file(&args[2]);
    let tokens = lex_or_die(&src, &args[2]);
    let program = match parser::parse(tokens) {
        Ok(p)  => p,
        Err(e) => {
            eprintln!("nuvc: {}: parse error: {}", args[2], e);
            process::exit(1);
        }
    };
    if check::check_program(&program, &args[2]) {
        process::exit(1);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// --fmt  :  pretty-print canonical Nuvola source
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_fmt(args: &[String]) {
    // nuvc --fmt <file> [--write]
    if args.len() < 3 {
        eprintln!("nuvc: --fmt requires a file argument");
        process::exit(1);
    }
    let src_path = &args[2];
    let write_in_place = args.get(3).map(|s| s == "--write").unwrap_or(false);

    let src    = read_file(src_path);
    let tokens = lex_or_die(&src, src_path);
    let program = match parser::parse(tokens) {
        Ok(p)  => p,
        Err(e) => {
            eprintln!("nuvc: {}: parse error: {}", src_path, e);
            process::exit(1);
        }
    };

    let formatted = fmt::format_program(&program);

    if write_in_place {
        if let Err(e) = fs::write(src_path, &formatted) {
            eprintln!("nuvc: cannot write `{}`: {}", src_path, e);
            process::exit(1);
        }
        eprintln!("nuvc: formatted `{}`", src_path);
    } else {
        print!("{}", formatted);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// compile  (M3: AST → C → binary via clang)
// ─────────────────────────────────────────────────────────────────────────────

fn cmd_compile(args: &[String]) {
    // Parse flags: nuvc <file> [-o <out>] [--emit-c]
    let src_path = &args[1];

    let mut out_path  = String::from("a.out");
    let mut emit_c    = false;
    let mut use_gc    = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("nuvc: -o requires an argument");
                    process::exit(1);
                }
                out_path = args[i].clone();
            }
            "--emit-c" => { emit_c = true; }
            "--gc"     => { use_gc = true; }
            flag => {
                eprintln!("nuvc: unknown flag `{}`", flag);
                process::exit(1);
            }
        }
        i += 1;
    }

    // ── 1. Lex ────────────────────────────────────────────────────────────────
    let src    = read_file(src_path);
    let tokens = lex_or_die(&src, src_path);

    // ── 2. Parse (spanned: captures source line per stmt for #line directives) ─
    let spanned = match parser::parse_spanned(tokens) {
        Ok(p)  => p,
        Err(e) => {
            eprintln!("nuvc: {}: parse error: {}", src_path, e);
            process::exit(1);
        }
    };
    let program: Vec<_> = spanned.iter().map(|(_, s)| s.clone()).collect();
    let _line_map: Vec<u32> = spanned.iter().map(|(l, _)| *l).collect();

    // ── 3. Codegen ────────────────────────────────────────────────────────────
    // Locate the runtime header relative to this binary or the source file.
    let runtime_dir = locate_runtime(src_path);
    let c_source = match codegen::emit(&program, &runtime_dir, src_path) {
        Ok(s)  => s,
        Err(e) => {
            eprintln!("nuvc: {}: codegen error: {}", src_path, e);
            process::exit(1);
        }
    };

    // ── 4. Write C source ─────────────────────────────────────────────────────
    if emit_c {
        // Just print the C and exit; useful for debugging.
        print!("{}", c_source);
        return;
    }

    // Write to a temp file beside the output binary.
    let c_path = format!("{}.c", out_path);
    if let Err(e) = fs::write(&c_path, &c_source) {
        eprintln!("nuvc: cannot write `{}`: {}", c_path, e);
        process::exit(1);
    }

    // ── 5. Invoke clang ───────────────────────────────────────────────────────
    let mut clang_args = vec![
        "-O3".to_string(),
        "-march=native".to_string(),
        "-ffast-math".to_string(),
        "-Wall".to_string(),
        "-Wno-unused-variable".to_string(),
        "-o".to_string(), out_path.clone(),
        c_path.clone(),
        format!("-I{}", runtime_dir),
        "-lm".to_string(),
        "-lpthread".to_string(),
    ];
    if use_gc {
        clang_args.push("-DNUVOLA_GC".to_string());
        clang_args.push("-lgc".to_string());
    }
    let status = process::Command::new("clang")
        .args(&clang_args)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("nuvc: failed to invoke clang: {}", e);
            eprintln!("      make sure clang is installed and on PATH");
            process::exit(1);
        });

    // ── 6. Clean up temp C file ───────────────────────────────────────────────
    let _ = fs::remove_file(&c_path);

    if !status.success() {
        eprintln!("nuvc: clang exited with status {}", status);
        process::exit(status.code().unwrap_or(1));
    }

    eprintln!("nuvc: compiled `{}` → `{}`", src_path, out_path);
}

/// Find the runtime/ directory that contains nuvola.h.
///
/// Search order:
///   1. Sibling of the source file: <src_dir>/runtime/
///   2. Relative to the binary:     <exe_dir>/runtime/  (for installed builds)
///   3. Fallback:                   "runtime/"           (CWD-relative)
fn locate_runtime(src_path: &str) -> String {
    // 1. Beside the source file.
    if let Some(parent) = Path::new(src_path).parent() {
        let candidate = parent.join("runtime");
        if candidate.join("nuvola.h").exists() {
            return candidate.to_string_lossy().into_owned();
        }
        // Also try one level up (common layout: tests/ next to runtime/).
        let candidate2 = parent.parent()
            .unwrap_or(parent)
            .join("runtime");
        if candidate2.join("nuvola.h").exists() {
            return candidate2.to_string_lossy().into_owned();
        }
    }

    // 2. Beside the running binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            let candidate = exe_dir.join("runtime");
            if candidate.join("nuvola.h").exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }

    // 3. CWD fallback.
    "runtime".to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────────────────

fn read_file(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("nuvc: cannot read `{}`: {}", path, e);
        process::exit(1);
    })
}

fn lex_or_die(src: &str, path: &str) -> Vec<token::Token> {
    lexer::tokenize(src).unwrap_or_else(|e| {
        eprintln!("nuvc: {}: lex error: {}", path, e);
        process::exit(1);
    })
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

fn print_help() {
    println!("nuvc — Nuvola Stage-0 Bootstrap Compiler  v0.5.0");
    println!();
    println!("USAGE:");
    println!("  nuvc --lex    <file>               dump token stream");
    println!("  nuvc --count  <file>               print token breakdown");
    println!("  nuvc --parse  <file>               parse and dump AST");
    println!("  nuvc --check  <file>               static analysis (arity, duplicates)");
    println!("  nuvc --fmt    <file>               pretty-print canonical source");
    println!("  nuvc --fmt    <file> --write       format file in-place");
    println!("  nuvc          <file> [-o out]      compile to native binary");
    println!("  nuvc          <file> --emit-c      print generated C source");
    println!("  nuvc          <file> --gc           compile with Boehm GC (requires libgc)");
    println!();
    println!("MILESTONES:");
    println!("  M1   Lexer         ✓");
    println!("  M2   Parser        ✓");
    println!("  M3   Codegen       ✓  (AST → C → binary via clang)");
    println!("  M13  Self-hosting  ✓  (codegen.nvl compiles itself)");
    println!("  M17  fmt           ✓  (nuvc --fmt)");
    println!("  M18  Stack traces  ✓  (runtime call chain on fatal errors)");
    println!("  M19  Check         ✓  (nuvc --check: arity + duplicate defs)");
}
