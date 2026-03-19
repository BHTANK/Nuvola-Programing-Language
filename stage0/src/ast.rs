/// Nuvola AST — Stage 0
///
/// All node types mirror `nuvola.peg` and the Python reference interpreter
/// (`tests/nuvola_eval.py` / `tests/nuvola_nuclear.py`).
///
/// Design:
///   • Recursive fields use `Box<T>` to bound node sizes.
///   • Spans are NOT stored in nodes — they live in the token stream.
///     The codegen/resolver can correlate nodes → tokens via source positions.
///   • `Vec<Stmt>` represents any sequence of statements (blocks, bodies, etc.)

// ─────────────────────────────────────────────────────────────────────────────
// Program
// ─────────────────────────────────────────────────────────────────────────────

pub type Program = Vec<Stmt>;

// ─────────────────────────────────────────────────────────────────────────────
// Statements
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Stmt {
    /// `x := expr`  (immutable)  /  `x = expr`  (mutable or reassign)
    /// `x: T := expr`  (typed immutable)
    Let {
        name:     String,
        type_ann: Option<TypeExpr>,
        mutable:  bool,
        value:    Box<Expr>,
    },

    /// `(a, b) := expr`  or  `a, b := expr`
    Destructure {
        names: Vec<String>,
        value: Box<Expr>,
    },

    /// `x = expr` where target is not a simple identifier:
    ///   `arr[i] = expr`  /  `obj.field = expr`
    Assign {
        target: AssignTarget,
        value:  Box<Expr>,
    },

    /// `x += expr`  etc.
    CompoundAssign {
        target: AssignTarget,
        op:     CompoundOp,
        value:  Box<Expr>,
    },

    /// `fn name(params) -> T => expr`  or indented block.
    FnDecl(FnDef),

    /// `async fn name(params) -> T => expr`  or indented block.
    AsyncFnDecl(FnDef),

    /// `if cond => stmt`  /  `if cond\n  block\n[elif...][else\n  block]`
    If {
        cond:         Box<Expr>,
        then_body:    Vec<Stmt>,
        elif_clauses: Vec<(Box<Expr>, Vec<Stmt>)>,
        else_body:    Option<Vec<Stmt>>,
    },

    /// `for var in iter => stmt`  or indented block.
    For {
        var:  ForVar,
        iter: Box<Expr>,
        body: Vec<Stmt>,
    },

    /// `while cond => stmt`  or indented block.
    While {
        cond: Box<Expr>,
        body: Vec<Stmt>,
    },

    /// `match expr\n  pattern => body\n  ...`
    Match {
        expr: Box<Expr>,
        arms: Vec<MatchArm>,
    },

    Return(Option<Box<Expr>>),
    Break(Option<Box<Expr>>),
    Continue,

    /// `type Name\n  field: T` (struct) or `type Name\n  Variant(T)` (enum).
    TypeDecl {
        name: String,
        kind: TypeDeclKind,
    },

    /// `trait Name\n  fn method(self, ...) -> T`
    TraitDecl {
        name:    String,
        methods: Vec<FnDef>,
    },

    /// `impl TraitName for TypeName\n  ...`  or  `impl TypeName\n  ...`
    ImplDecl {
        trait_name: Option<String>,
        type_name:  String,
        methods:    Vec<FnDef>,
    },

    /// `comptime NAME := expr`
    Comptime {
        name:  String,
        value: Box<Expr>,
    },

    /// `extern "lib"? fn name(params) -> T`
    ExternFn {
        lib:      Option<String>,
        name:     String,
        params:   Vec<Param>,
        ret_type: Option<TypeExpr>,
    },

    /// `unsafe\n  body`
    Unsafe(Vec<Stmt>),

    /// `await expr`  (statement position)
    AwaitStmt(Box<Expr>),

    /// `spawn expr`  (statement position)
    SpawnStmt(Box<Expr>),

    /// `throw expr`
    Throw(Box<Expr>),

    /// `try\n  body\ncatch var\n  handler`
    TryCatch {
        body:    Vec<Stmt>,
        catches: Vec<(String, Vec<Stmt>)>,
    },

    /// `import module.path.{names} as alias`
    Import {
        path:  Vec<String>,
        names: Option<Vec<String>>,
        alias: Option<String>,
    },

    /// `@name\nstmt`
    Annotation {
        name:  String,
        inner: Box<Stmt>,
    },

    /// Bare expression used as a statement (e.g., function call for side-effects).
    Expr(Box<Expr>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Expressions
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Expr {
    // ── Literals ─────────────────────────────────────────────────────────────
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Nil,
    Self_,

    // ── Identifier ───────────────────────────────────────────────────────────
    Ident(String),

    // ── Binary operations ─────────────────────────────────────────────────────
    BinOp {
        op:  BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    // ── Unary operations ──────────────────────────────────────────────────────
    UnOp {
        op:   UnOp,
        expr: Box<Expr>,
    },

    // ── Pipeline  `lhs |> rhs` ────────────────────────────────────────────────
    Pipe {
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },

    // ── Range  `start..end`  or  `start..=end` ───────────────────────────────
    Range {
        start:     Box<Expr>,
        end:       Box<Expr>,
        inclusive: bool,
    },

    // ── Function / method calls ───────────────────────────────────────────────
    Call {
        callee: Box<Expr>,
        args:   Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
    },
    MethodCall {
        obj:    Box<Expr>,
        method: String,
        args:   Vec<Expr>,
        kwargs: Vec<(String, Expr)>,
    },

    // ── Postfix ───────────────────────────────────────────────────────────────
    Index { obj: Box<Expr>, idx:   Box<Expr> },
    Field { obj: Box<Expr>, field: String    },
    OptChain { obj: Box<Expr>, field: String },

    // ── Collections ───────────────────────────────────────────────────────────
    List(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
    Set(Vec<Expr>),
    Tuple(Vec<Expr>),

    // ── Struct literal  `Name { field: val, .. }` ────────────────────────────
    Struct {
        name:   String,
        fields: Vec<StructField>,
    },

    // ── Functions ─────────────────────────────────────────────────────────────
    /// Anonymous lambda: `fn(params) => body` or block.
    Lambda(Box<FnDef>),
    /// Placeholder lambda: `_`, `_ + 1`, `_.field`
    Placeholder(Option<Box<PlaceholderOp>>),

    // ── Control flow (expression form) ────────────────────────────────────────
    If {
        cond:         Box<Expr>,
        then_expr:    Box<Expr>,
        elif_clauses: Vec<(Box<Expr>, Box<Expr>)>,
        else_expr:    Option<Box<Expr>>,
    },
    Match {
        expr: Box<Expr>,
        arms: Vec<MatchArm>,
    },

    // ── Nuclear ───────────────────────────────────────────────────────────────
    Await(Box<Expr>),
    Spawn(Box<Expr>),
    Unsafe(Vec<Stmt>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Operators
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BinOp {
    // Arithmetic
    Add, Sub, Mul, Div, IntDiv, Mod, Pow,
    // Comparison
    Eq, Ne, Lt, Gt, Le, Ge,
    // Logic
    And, Or,
    // Identity / type check
    Is,
    // Matrix multiply
    Matmul,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnOp { Neg, Not }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompoundOp { Add, Sub, Mul, Div }

// ─────────────────────────────────────────────────────────────────────────────
// Assignment
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AssignTarget {
    Ident(String),
    Index { obj: Box<Expr>, idx:   Box<Expr> },
    Field { obj: Box<Expr>, field: String    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Functions
// ─────────────────────────────────────────────────────────────────────────────

/// A complete function definition (named or anonymous, sync or async).
#[derive(Debug, Clone)]
pub struct FnDef {
    pub name:           Option<String>,
    pub generic_params: Vec<GenericParam>,
    pub params:         Vec<Param>,
    pub ret_type:       Option<TypeExpr>,
    pub where_clause:   Vec<(String, Vec<String>)>,
    pub body:           FnBody,
}

#[derive(Debug, Clone)]
pub enum FnBody {
    /// `=> expr`
    Arrow(Box<Expr>),
    /// Indented block of statements.
    Block(Vec<Stmt>),
    /// No body — trait method signature only.
    Abstract,
}

/// One parameter in a function signature.
#[derive(Debug, Clone)]
pub struct Param {
    pub name:     String,
    pub type_ann: Option<TypeExpr>,
    pub default:  Option<Box<Expr>>,
    pub variadic: bool,   // `...name` — rest parameter
}

/// `<T: Bound + Other>`
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name:   String,
    pub bounds: Vec<String>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Match
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard:   Option<Box<Expr>>,
    pub body:    MatchBody,
}

#[derive(Debug, Clone)]
pub enum MatchBody {
    Expr(Box<Expr>),
    Block(Vec<Stmt>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Patterns
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Pattern {
    /// `_`
    Wildcard,
    /// Any literal value: integer, float, string, bool, nil.
    Literal(Box<Expr>),
    /// `-5`  (negated integer — common in match arms).
    NegInt(i64),
    /// `1..10`  or  `1..=10`
    Range {
        start:     i64,
        end:       Option<i64>,
        inclusive: bool,
    },
    /// `Some(p)`
    SomePat(Box<Pattern>),
    /// `None` / `nil`
    NonePat,
    /// `Ok(p)`
    OkPat(Box<Pattern>),
    /// `Err(p)`
    ErrPat(Box<Pattern>),
    /// `Name` or `Name.Variant` or `Name(p, ...)`
    Ctor {
        name:    String,
        variant: Option<String>,
        args:    Vec<Pattern>,
    },
    /// Lowercase identifier — variable capture.
    Bind(String),
    /// `p1 | p2 | ...`
    Or(Vec<Pattern>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Type expressions
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TypeExpr {
    /// `Ident`  or  `Ident<T, U>`
    Named(String, Vec<TypeExpr>),
    /// `(T, U)`
    Tuple(Vec<TypeExpr>),
    /// `[T]`
    List(Box<TypeExpr>),
    /// `&T`
    Ref(Box<TypeExpr>),
    /// `?T`
    Option(Box<TypeExpr>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Type declarations
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum TypeDeclKind {
    /// All members are `ident: Type` field declarations → struct.
    Struct(Vec<FieldDecl>),
    /// All members are `Variant(T, ...)` declarations → enum.
    Enum(Vec<VariantDecl>),
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name:     String,
    pub type_ann: TypeExpr,
    pub default:  Option<Box<Expr>>,
}

#[derive(Debug, Clone)]
pub struct VariantDecl {
    pub name:   String,
    pub fields: Vec<VariantField>,
}

#[derive(Debug, Clone)]
pub struct VariantField {
    pub name:     Option<String>,
    pub type_ann: TypeExpr,
}

// ─────────────────────────────────────────────────────────────────────────────
// For-loop variable
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ForVar {
    Simple(String),
    Tuple(Vec<String>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Struct literal fields
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum StructField {
    /// `name: expr`  or shorthand `name` (= `name: name`)
    Named { name: String, value: Box<Expr> },
    /// `..base_expr`
    Spread(Box<Expr>),
}

// ─────────────────────────────────────────────────────────────────────────────
// Placeholder lambda
// ─────────────────────────────────────────────────────────────────────────────

/// The optional "tail" attached to a `_` placeholder.
///
///   `_`       → `Placeholder(None)`        → identity   fn(x) => x
///   `_ + 1`   → `Placeholder(Some(Bin(Add, Int(1))))` → fn(x) => x + 1
///   `_.field` → `Placeholder(Some(Field("field")))`   → fn(x) => x.field
#[derive(Debug, Clone)]
pub enum PlaceholderOp {
    Bin(BinOp, Box<Expr>),
    Field(String),
}
