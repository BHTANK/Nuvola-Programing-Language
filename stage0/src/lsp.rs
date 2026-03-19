/// nuvola-lsp — Nuvola Language Server
///
/// Provides: diagnostics, go-to-definition, hover, document symbols, completion.
/// Communicates via LSP over stdio.

use std::collections::HashMap;
use std::error::Error;

use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::*;
use serde_json::Value;

// Re-use the compiler's lexer, parser, and AST
use nuvc::ast::{self, Expr, FnBody, FnDef, Stmt};
use nuvc::lexer;
use nuvc::parser;

// ─────────────────────────────────────────────────────────────────────────────
// Main
// ─────────────────────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    eprintln!("nuvola-lsp: starting");

    let (connection, io_threads) = Connection::stdio();

    let server_caps = ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(
            TextDocumentSyncKind::FULL,
        )),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".into(), "|".into()]),
            ..Default::default()
        }),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        ..Default::default()
    };

    let init_params = serde_json::to_value(server_caps)?;
    connection.initialize(init_params)?;

    eprintln!("nuvola-lsp: initialized");

    let mut state = ServerState::new();
    main_loop(&connection, &mut state)?;

    io_threads.join()?;
    eprintln!("nuvola-lsp: shutdown");
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Server state
// ─────────────────────────────────────────────────────────────────────────────

struct ServerState {
    /// URI → source text
    docs: HashMap<String, String>,
    /// URI → parsed symbols (function defs with line info)
    symbols: HashMap<String, Vec<SymbolInfo>>,
}

#[derive(Clone)]
struct SymbolInfo {
    name: String,
    kind: SymbolKind,
    line: u32,   // 0-based
    col: u32,    // 0-based
    params: Vec<String>,
    detail: String,
}

impl ServerState {
    fn new() -> Self {
        ServerState {
            docs: HashMap::new(),
            symbols: HashMap::new(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Main loop
// ─────────────────────────────────────────────────────────────────────────────

fn main_loop(
    conn: &Connection,
    state: &mut ServerState,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    for msg in &conn.receiver {
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    return Ok(());
                }
                handle_request(conn, state, req)?;
            }
            Message::Notification(notif) => {
                handle_notification(conn, state, notif)?;
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Notifications (document open/change/close)
// ─────────────────────────────────────────────────────────────────────────────

fn handle_notification(
    conn: &Connection,
    state: &mut ServerState,
    notif: Notification,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match notif.method.as_str() {
        "textDocument/didOpen" => {
            let params: DidOpenTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri.to_string();
            let text = params.text_document.text;
            state.docs.insert(uri.clone(), text.clone());
            update_diagnostics(conn, state, &uri, &text)?;
        }
        "textDocument/didChange" => {
            let params: DidChangeTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri.to_string();
            if let Some(change) = params.content_changes.into_iter().last() {
                state.docs.insert(uri.clone(), change.text.clone());
                update_diagnostics(conn, state, &uri, &change.text)?;
            }
        }
        "textDocument/didClose" => {
            let params: DidCloseTextDocumentParams = serde_json::from_value(notif.params)?;
            let uri = params.text_document.uri.to_string();
            state.docs.remove(&uri);
            state.symbols.remove(&uri);
            // Clear diagnostics
            let diag_params = PublishDiagnosticsParams {
                uri: params.text_document.uri,
                diagnostics: vec![],
                version: None,
            };
            conn.sender.send(Message::Notification(Notification {
                method: "textDocument/publishDiagnostics".into(),
                params: serde_json::to_value(diag_params)?,
            }))?;
        }
        _ => {}
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Requests (completion, hover, definition, symbols)
// ─────────────────────────────────────────────────────────────────────────────

fn handle_request(
    conn: &Connection,
    state: &mut ServerState,
    req: Request,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    match req.method.as_str() {
        "textDocument/completion" => {
            let result = completion_list();
            send_response(conn, req.id, result)?;
        }
        "textDocument/hover" => {
            let params: HoverParams = serde_json::from_value(req.params)?;
            let result = hover(state, &params);
            send_response(conn, req.id, result)?;
        }
        "textDocument/definition" => {
            let params: GotoDefinitionParams = serde_json::from_value(req.params)?;
            let result = goto_definition(state, &params);
            send_response(conn, req.id, result)?;
        }
        "textDocument/documentSymbol" => {
            let params: DocumentSymbolParams = serde_json::from_value(req.params)?;
            let result = document_symbols(state, &params);
            send_response(conn, req.id, result)?;
        }
        _ => {
            let resp = Response::new_err(req.id, -32601, "method not found".into());
            conn.sender.send(Message::Response(resp))?;
        }
    }
    Ok(())
}

fn send_response(
    conn: &Connection,
    id: RequestId,
    result: impl serde::Serialize,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let resp = Response::new_ok(id, serde_json::to_value(result)?);
    conn.sender.send(Message::Response(resp))?;
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Diagnostics — lex + parse, report errors
// ─────────────────────────────────────────────────────────────────────────────

fn update_diagnostics(
    conn: &Connection,
    state: &mut ServerState,
    uri: &str,
    text: &str,
) -> Result<(), Box<dyn Error + Sync + Send>> {
    let mut diagnostics = Vec::new();

    match lexer::tokenize(text) {
        Err(e) => {
            diagnostics.push(Diagnostic {
                range: Range {
                    start: Position { line: e.line.saturating_sub(1), character: e.col.saturating_sub(1) },
                    end:   Position { line: e.line.saturating_sub(1), character: e.col.saturating_sub(1) + 10 },
                },
                severity: Some(DiagnosticSeverity::ERROR),
                message: e.msg.clone(),
                source: Some("nuvola".into()),
                ..Default::default()
            });
        }
        Ok(tokens) => {
            match parser::parse(tokens) {
                Err(e) => {
                    diagnostics.push(Diagnostic {
                        range: Range {
                            start: Position { line: e.line.saturating_sub(1), character: e.col.saturating_sub(1) },
                            end:   Position { line: e.line.saturating_sub(1), character: e.col.saturating_sub(1) + 10 },
                        },
                        severity: Some(DiagnosticSeverity::ERROR),
                        message: e.msg.clone(),
                        source: Some("nuvola".into()),
                        ..Default::default()
                    });
                }
                Ok(program) => {
                    // Parse succeeded — extract symbols
                    let syms = extract_symbols(&program);
                    state.symbols.insert(uri.to_string(), syms);
                }
            }
        }
    }

    let parsed_uri: Url = uri.parse().unwrap_or_else(|_| Url::parse("file:///unknown").unwrap());
    let diag_params = PublishDiagnosticsParams {
        uri: parsed_uri,
        diagnostics,
        version: None,
    };
    conn.sender.send(Message::Notification(Notification {
        method: "textDocument/publishDiagnostics".into(),
        params: serde_json::to_value(diag_params)?,
    }))?;

    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// Symbol extraction — walk the AST for function defs, type defs, etc.
// ─────────────────────────────────────────────────────────────────────────────

fn extract_symbols(program: &[Stmt]) -> Vec<SymbolInfo> {
    let mut syms = Vec::new();
    for stmt in program {
        extract_from_stmt(stmt, &mut syms);
    }
    syms
}

fn extract_from_stmt(stmt: &Stmt, syms: &mut Vec<SymbolInfo>) {
    match stmt {
        Stmt::FnDecl(def) | Stmt::AsyncFnDecl(def) => {
            if let Some(name) = &def.name {
                let params: Vec<String> = def.params.iter().map(|p| p.name.clone()).collect();
                let sig = format!("fn {}({})", name, params.join(", "));
                syms.push(SymbolInfo {
                    name: name.clone(),
                    kind: SymbolKind::FUNCTION,
                    line: 0, // We don't have span info on Stmts directly
                    col: 0,
                    params,
                    detail: sig,
                });
            }
        }
        Stmt::TypeDecl { name, .. } => {
            syms.push(SymbolInfo {
                name: name.clone(),
                kind: SymbolKind::STRUCT,
                line: 0,
                col: 0,
                params: vec![],
                detail: format!("type {}", name),
            });
        }
        Stmt::TraitDecl { name, .. } => {
            syms.push(SymbolInfo {
                name: name.clone(),
                kind: SymbolKind::INTERFACE,
                line: 0,
                col: 0,
                params: vec![],
                detail: format!("trait {}", name),
            });
        }
        Stmt::ImplDecl { type_name, .. } => {
            syms.push(SymbolInfo {
                name: type_name.clone(),
                kind: SymbolKind::CLASS,
                line: 0,
                col: 0,
                params: vec![],
                detail: format!("impl {}", type_name),
            });
        }
        _ => {}
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Completion — keywords + builtins
// ─────────────────────────────────────────────────────────────────────────────

fn completion_list() -> CompletionList {
    let mut items = Vec::new();

    // Keywords
    for kw in &[
        "fn", "if", "elif", "else", "for", "while", "loop", "match",
        "return", "break", "continue", "import", "export", "from", "as",
        "type", "trait", "impl", "class", "async", "await", "spawn",
        "comptime", "extern", "unsafe", "try", "catch", "throw", "where",
        "and", "or", "not", "is", "in",
        "true", "false", "nil", "None", "self",
    ] {
        items.push(CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }

    // Builtins
    let builtins = &[
        ("print", "print(value) -- Print to stdout"),
        ("assert", "assert(cond, msg) -- Assert condition"),
        ("len", "len(value) -- Length of list/string/map"),
        ("str", "str(value) -- Convert to string"),
        ("int", "int(value) -- Convert to integer"),
        ("float", "float(value) -- Convert to float"),
        ("bool", "bool(value) -- Convert to boolean"),
        ("type", "type(value) -- Get type name"),
        ("range", "range(n) or range(start, end) -- Integer range"),
        ("map", "map(fn, list) -- Apply fn to each element"),
        ("filter", "filter(fn, list) -- Keep elements where fn is true"),
        ("sum", "sum(list) -- Sum of list elements"),
        ("sorted", "sorted(list) -- Return sorted copy"),
        ("reversed", "reversed(list) -- Return reversed copy"),
        ("abs", "abs(n) -- Absolute value"),
        ("sqrt", "sqrt(n) -- Square root"),
        ("max", "max(a, b) or max(list) -- Maximum"),
        ("min", "min(a, b) or min(list) -- Minimum"),
        ("join", "join(sep, list) -- Join strings"),
        ("split", "split(str, delim) -- Split string"),
        ("push", "list.push(item) -- Append to list"),
        ("pop", "list.pop() -- Remove and return last"),
        ("contains", "list.contains(val) -- Check membership"),
        ("take", "list.take(n) -- First n elements"),
        ("drop", "list.drop(n) -- Skip first n elements"),
        ("sha256", "sha256(str) -- SHA-256 hash"),
        ("hmac_sha256", "hmac_sha256(key, msg) -- HMAC-SHA256"),
        ("base64_encode", "base64_encode(str) -- Base64 encode"),
        ("base64_decode", "base64_decode(str) -- Base64 decode"),
        ("json_parse", "json_parse(str) -- Parse JSON string"),
        ("json_stringify", "json_stringify(val) -- Serialize to JSON"),
        ("http_get", "http_get(url) -- HTTP GET request"),
        ("http_post", "http_post(url, body) -- HTTP POST request"),
        ("http_serve", "http_serve(port, handler) -- Start HTTP server"),
        ("regex_match", "regex_match(pattern, str) -- Test regex match"),
        ("regex_find", "regex_find(pattern, str) -- Find first match"),
        ("regex_find_all", "regex_find_all(pattern, str) -- Find all matches"),
        ("regex_replace", "regex_replace(pattern, str, repl) -- Replace matches"),
        ("time_ms", "time_ms() -- Current time in milliseconds"),
        ("sleep_ms", "sleep_ms(n) -- Sleep for n milliseconds"),
        ("spawn", "spawn(fn, arg) -- Spawn a thread"),
        ("channel", "channel() -- Create a channel"),
        ("parallel_map", "parallel_map(fn, list) -- Map across CPU cores"),
        ("parallel_for", "parallel_for(range, fn) -- For-each across CPU cores"),
        ("cpu_count", "cpu_count() -- Number of CPU cores"),
        ("read_file", "read_file(path) -- Read file contents"),
        ("write_file", "write_file(path, content) -- Write file"),
        ("sin", "sin(x) -- Sine"),
        ("cos", "cos(x) -- Cosine"),
        ("tan", "tan(x) -- Tangent"),
        ("exp", "exp(x) -- e^x"),
        ("log", "log(x) -- Natural logarithm"),
        ("pow", "pow(base, exp) -- Power"),
        ("floor", "floor(x) -- Floor"),
        ("ceil", "ceil(x) -- Ceiling"),
        ("round", "round(x) -- Round"),
        ("Some", "Some(value) -- Option with value"),
        ("Ok", "Ok(value) -- Result success"),
        ("Err", "Err(value) -- Result error"),
    ];

    for (name, doc) in builtins {
        items.push(CompletionItem {
            label: name.to_string(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(doc.to_string()),
            ..Default::default()
        });
    }

    CompletionList {
        is_incomplete: false,
        items,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Hover — show function signature
// ─────────────────────────────────────────────────────────────────────────────

fn hover(state: &ServerState, params: &HoverParams) -> Option<Hover> {
    let uri = params.text_document_position_params.text_document.uri.to_string();
    let pos = params.text_document_position_params.position;
    let text = state.docs.get(&uri)?;

    // Find the word at the cursor position
    let word = word_at_position(text, pos)?;

    // Check user-defined symbols
    if let Some(syms) = state.symbols.get(&uri) {
        for sym in syms {
            if sym.name == word {
                return Some(Hover {
                    contents: HoverContents::Markup(MarkupContent {
                        kind: MarkupKind::Markdown,
                        value: format!("```nuvola\n{}\n```", sym.detail),
                    }),
                    range: None,
                });
            }
        }
    }

    // Check builtins
    let builtin_doc = builtin_hover(&word);
    if let Some(doc) = builtin_doc {
        return Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: doc,
            }),
            range: None,
        });
    }

    None
}

fn builtin_hover(name: &str) -> Option<String> {
    let doc = match name {
        "print"        => "```nuvola\nprint(value)\n```\nPrint value to stdout with newline.",
        "assert"       => "```nuvola\nassert(condition, message?)\n```\nAbort if condition is false.",
        "len"          => "```nuvola\nlen(value) -> int\n```\nLength of list, string, or map.",
        "range"        => "```nuvola\nrange(n) or range(start, end)\n```\nGenerate integer range `[start, end)`.",
        "map"          => "```nuvola\nmap(fn, list) -> list\n```\nApply function to each element.",
        "filter"       => "```nuvola\nfilter(fn, list) -> list\n```\nKeep elements where fn returns true.",
        "sum"          => "```nuvola\nsum(list) -> number\n```\nSum all elements.",
        "sorted"       => "```nuvola\nsorted(list) -> list\n```\nReturn a sorted copy.",
        "reversed"     => "```nuvola\nreversed(list) -> list\n```\nReturn a reversed copy.",
        "sha256"       => "```nuvola\nsha256(str) -> str\n```\nSHA-256 hash (hex string).",
        "hmac_sha256"  => "```nuvola\nhmac_sha256(key, message) -> str\n```\nHMAC-SHA256 authentication code.",
        "json_parse"   => "```nuvola\njson_parse(str) -> value\n```\nParse JSON string into Nuvola value.",
        "json_stringify" => "```nuvola\njson_stringify(value) -> str\n```\nSerialize value to JSON string.",
        "parallel_map" => "```nuvola\nparallel_map(fn, list) -> list\n```\nMap function across all CPU cores using thread pool.",
        "parallel_for" => "```nuvola\nparallel_for(range, fn)\n```\nExecute fn(i) for each item across CPU cores.",
        "cpu_count"    => "```nuvola\ncpu_count() -> int\n```\nNumber of available CPU cores.",
        "spawn"        => "```nuvola\nspawn(fn, arg) -> future\n```\nSpawn a new thread.",
        "channel"      => "```nuvola\nchannel() -> chan\n```\nCreate a thread-safe channel.",
        "http_serve"   => "```nuvola\nhttp_serve(port, handler)\n```\nStart HTTP server on port. Handler receives request map.",
        "time_ms"      => "```nuvola\ntime_ms() -> int\n```\nCurrent time in milliseconds since epoch.",
        _ => return None,
    };
    Some(doc.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// Go-to-definition
// ─────────────────────────────────────────────────────────────────────────────

fn goto_definition(
    state: &ServerState,
    params: &GotoDefinitionParams,
) -> Option<GotoDefinitionResponse> {
    let uri = params.text_document_position_params.text_document.uri.to_string();
    let pos = params.text_document_position_params.position;
    let text = state.docs.get(&uri)?;
    let word = word_at_position(text, pos)?;

    // Search for the function definition line in the source text
    // (more reliable than AST line numbers since we don't track spans on Stmts)
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        // Match "fn <name>(" or "async fn <name>("
        if (trimmed.starts_with(&format!("fn {}(", word))
            || trimmed.starts_with(&format!("fn {}", word))
            || trimmed.starts_with(&format!("async fn {}(", word)))
            && trimmed.contains(&word)
        {
            let col = line.find(&format!("fn {}", word)).unwrap_or(0);
            let target_uri: Url = uri.parse().ok()?;
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: target_uri,
                range: Range {
                    start: Position { line: i as u32, character: col as u32 },
                    end: Position { line: i as u32, character: (col + word.len() + 3) as u32 },
                },
            }));
        }
        // Match "type <Name>" or "trait <Name>"
        if (trimmed.starts_with(&format!("type {}", word))
            || trimmed.starts_with(&format!("trait {}", word))
            || trimmed.starts_with(&format!("impl {}", word)))
        {
            let col = line.find(&word).unwrap_or(0);
            let target_uri: Url = uri.parse().ok()?;
            return Some(GotoDefinitionResponse::Scalar(Location {
                uri: target_uri,
                range: Range {
                    start: Position { line: i as u32, character: col as u32 },
                    end: Position { line: i as u32, character: (col + word.len()) as u32 },
                },
            }));
        }
    }

    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Document symbols (outline)
// ─────────────────────────────────────────────────────────────────────────────

fn document_symbols(
    state: &ServerState,
    params: &DocumentSymbolParams,
) -> Option<DocumentSymbolResponse> {
    let uri = params.text_document.uri.to_string();
    let text = state.docs.get(&uri)?;
    let lines: Vec<&str> = text.lines().collect();

    let mut symbols: Vec<SymbolInformation> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        // fn <name>(
        if let Some(rest) = trimmed.strip_prefix("fn ") {
            if let Some(paren) = rest.find('(') {
                let name = &rest[..paren];
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    #[allow(deprecated)]
                    symbols.push(SymbolInformation {
                        name: name.to_string(),
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: params.text_document.uri.clone(),
                            range: Range {
                                start: Position { line: i as u32, character: 0 },
                                end: Position { line: i as u32, character: line.len() as u32 },
                            },
                        },
                        container_name: None,
                    });
                }
            }
        }

        // async fn <name>(
        if let Some(rest) = trimmed.strip_prefix("async fn ") {
            if let Some(paren) = rest.find('(') {
                let name = &rest[..paren];
                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    #[allow(deprecated)]
                    symbols.push(SymbolInformation {
                        name: name.to_string(),
                        kind: SymbolKind::FUNCTION,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: params.text_document.uri.clone(),
                            range: Range {
                                start: Position { line: i as u32, character: 0 },
                                end: Position { line: i as u32, character: line.len() as u32 },
                            },
                        },
                        container_name: None,
                    });
                }
            }
        }

        // type <Name> or trait <Name>
        for prefix in &["type ", "trait ", "impl "] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !name.is_empty() {
                    let kind = match *prefix {
                        "type " => SymbolKind::STRUCT,
                        "trait " => SymbolKind::INTERFACE,
                        "impl " => SymbolKind::CLASS,
                        _ => SymbolKind::OBJECT,
                    };
                    #[allow(deprecated)]
                    symbols.push(SymbolInformation {
                        name: name.clone(),
                        kind,
                        tags: None,
                        deprecated: None,
                        location: Location {
                            uri: params.text_document.uri.clone(),
                            range: Range {
                                start: Position { line: i as u32, character: 0 },
                                end: Position { line: i as u32, character: line.len() as u32 },
                            },
                        },
                        container_name: None,
                    });
                }
            }
        }
    }

    Some(DocumentSymbolResponse::Flat(symbols))
}

// ─────────────────────────────────────────────────────────────────────────────
// Utilities
// ─────────────────────────────────────────────────────────────────────────────

fn word_at_position(text: &str, pos: Position) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let line = lines.get(pos.line as usize)?;
    let col = pos.character as usize;
    if col > line.len() {
        return None;
    }

    let bytes = line.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';

    // Find word start
    let mut start = col;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }

    // Find word end
    let mut end = col;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }

    if start == end {
        return None;
    }

    Some(line[start..end].to_string())
}
