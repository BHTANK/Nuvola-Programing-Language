# Nuvola Compiler Architecture

## Design Goals

1. **Fast**: A 100K LOC project compiles in under 1 second from clean, under 50ms incremental
2. **Parallel**: Every compilation stage is parallelized across CPU cores
3. **Incremental**: Only changed modules and their dependents are recompiled
4. **Smart**: The compiler understands the whole program and makes radical optimizations
5. **Transparent**: Every optimization decision can be explained via `nvc --explain`

---

## Pipeline Overview

```
Source Files (.nvl)
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│  FRONTEND                                              ~15ms │
│                                                             │
│  Lexer / Parser           → AST (per file, parallel)       │
│  Name Resolution           → Resolved AST                  │
│  Type Checking             → Typed AST                     │
│  Effect Checking           → Effect-annotated AST          │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│  WHOLE-PROGRAM ANALYSIS                               ~20ms │
│                                                             │
│  Monomorphization          → Specialize all generics       │
│  Global Type Inference     → Resolve all unknowns          │
│  Dependency Graph Build    → Data flow analysis            │
│  Escape Analysis           → Stack vs heap decisions       │
│  Region Inference          → Memory region assignment      │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│  MID-LEVEL IR (NvIR)                                  ~10ms │
│                                                             │
│  SSA Construction          → Static single-assignment form │
│  Comptime Evaluation       → Run @comptime blocks          │
│  Macro Expansion           → Run @macro blocks             │
│  Borrow Check              → Ownership verification        │
│  GPU Kernel Extraction     → Identify @gpu functions       │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│  OPTIMIZATION PASSES                                  ~30ms │
│                                                             │
│  APX Pass                  → Automatic parallelism         │
│  Inlining                  → Inline hot/small functions    │
│  Dead Code Elimination     → Remove unreachable code       │
│  Constant Folding          → Evaluate constants            │
│  Loop Optimization         → Unroll, vectorize, fuse       │
│  Memory Layout             → Field reordering, padding     │
│  Alias Analysis            → Enable more optimizations     │
│  Tail Call Optimization    → Convert recursion to loops    │
│  Profile-Guided (if PGO)   → Use runtime profile data      │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│  BACKEND CODEGEN                                      ~25ms │
│                                                             │
│  Native: LLVM IR → Optimized machine code                  │
│  WASM:   Wasm IR → .wasm binary                            │
│  GPU:    PTX IR  → .ptx / .cubin (CUDA)                    │
│          MSL     → .metallib (Apple Metal)                 │
│          SPIR-V  → Vulkan compute shaders                  │
│  Dist:   Task graph serialization                          │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
┌─────────────────────────────────────────────────────────────┐
│  LINKER                                                ~5ms │
│                                                             │
│  LTO: Link-time optimization across object files           │
│  Strip: Remove debug info for release builds               │
│  Sign:  Code signing (macOS, Windows)                      │
└─────────────────────────────────────────────────────────────┘
       │
       ▼
  Native Binary / .wasm / GPU kernels / Distribution package
```

---

## 1. Lexer and Parser

The lexer is a hand-written DFA (deterministic finite automaton) — faster than any generated
lexer. It tokenizes 150MB of source code per second on a single core.

The parser is a Pratt parser (top-down operator precedence) with a few recursive-descent
extensions for complex constructs. It builds a **concrete syntax tree** (CST) that preserves
whitespace and comments, enabling perfect error recovery and formatting.

Parsing is **parallel per file**: all files in a package are parsed simultaneously.

```
File A ─── Parser A ─┐
File B ─── Parser B ─┤── CST Merger ── Package CST
File C ─── Parser C ─┘
```

**Error recovery**: the parser never gives up. It inserts error nodes and continues parsing.
A file with 10 errors still produces a valid AST for the rest of the file.

---

## 2. Name Resolution

Name resolution converts identifiers to their canonical definitions. It handles:
- Shadowing
- Import resolution (including remote packages)
- Macro hygiene (macro-generated names cannot capture external names)
- Module visibility rules

Name resolution is done in a two-pass system:
1. **Declaration pass**: collect all top-level declarations in all modules (parallel)
2. **Reference pass**: resolve all uses (parallel per file, with a shared read-only symbol table)

---

## 3. Type Inference and Checking

Nuvola uses **Hindley-Milner with extensions** for its type inference:

```
Algorithm W (classic HM)
  + Bidirectional type checking (synthesize + check modes)
  + Row polymorphism (structural record typing)
  + Constraint-based dependent type checking
  + Effect type propagation
  + Temporal type tracking
```

Type inference is **whole-program**: all files are processed together in topological
dependency order. This allows:
- Cross-module type inference
- Better error messages ("type came from module X, function Y")
- Global optimization opportunities

**Inference complexity**: O(n · α(n)) amortized, where α is the inverse Ackermann function.
In practice: linear in the number of expressions.

---

## 4. The APX Pass (Automatic Parallelism eXtraction)

This is Nuvola's most distinctive compilation pass. It analyzes the entire program's
data dependency graph and schedules independent computations for parallel execution.

### How it works

1. **Effect annotation collection**: gather all `@pure`, `@io`, `@mut` annotations
2. **Alias analysis**: determine which expressions can alias each other
3. **Dependency graph construction**: for each expression E, find all expressions that
   E depends on (reads a value that E writes)
4. **Topological sort**: produce a partial order of expressions
5. **Critical path analysis**: find the minimum-time schedule for the available cores
6. **Code generation**: insert synchronization points and parallel spawn instructions

### Example

```nuvola
-- Source code:
fn analyze(data: [Row]) -> Report
  a := step_a(data)     -- depends on: data
  b := step_b(a)        -- depends on: a
  c := step_c(a)        -- depends on: a
  d := step_d(b, c)     -- depends on: b, c
  e := step_e(data)     -- depends on: data
  Report { d, e }       -- depends on: d, e

-- APX-generated schedule (4 cores):
-- T=0: [step_a(data), step_e(data)]  -- parallel (both only need data)
-- T=1: [step_b(a), step_c(a)]        -- parallel (both only need a, which is ready)
-- T=2: [step_d(b, c)]                -- sequential (needs both b and c)
-- T=3: Report { d, e }               -- sequential (needs d and e)
```

### Constraints

APX only parallelizes **pure** functions. Impure functions (`@io`, `@mut`) are kept
in their original sequential order. This is sound: parallelizing I/O could change
observable program behavior.

---

## 5. Escape Analysis and Region Inference

Escape analysis determines whether each heap allocation can be:
- **Stack-promoted**: never leaves the function → lives on stack
- **Region-allocated**: lives within a bounded scope → freed atomically
- **Individually heap-allocated**: outlives its creator → needs Box/Arc

The analysis is flow-sensitive and interprocedural. It handles:
- Return values (may escape)
- Closures (capture analysis)
- Channel sends (escape to receiver)
- Trait objects (may escape to dynamic dispatch target)

**Region inference** groups non-escaping heap allocations into regions with
the same lifetime. Each region becomes a bump allocator — O(1) allocation and deallocation.

---

## 6. GPU Kernel Extraction

Functions annotated with `@gpu` are compiled to a separate GPU kernel:

1. **Extraction**: the function body is copied to a separate compilation unit
2. **Thread model mapping**: the function's iteration structure is mapped to
   GPU threads, warps, and blocks
3. **Memory model translation**: references to host data become buffer bindings
4. **Shared memory optimization**: frequently accessed data is promoted to shared memory
5. **Warp divergence analysis**: branches that cause warp divergence are restructured

The compiler generates optimal thread layouts:
```
-- fn(data: Tensor[f32, N]) -> Tensor[f32, N] where N = 1024*1024
-- → 1024 blocks × 1024 threads/block = 1,048,576 parallel GPU threads
-- → automatically uses shared memory for intermediate results
-- → generates vectorized memory accesses (128-bit loads)
```

---

## 7. Incremental Compilation

Every compilation artifact is content-addressed (keyed by the hash of its inputs).
The cache stores:
- Parsed ASTs
- Type-checked ASTs
- Compiled object files
- Linked artifacts

A rebuild only recompiles modules whose inputs (source or dependencies) changed.
The build system tracks fine-grained dependencies at the **declaration level**:
changing a private helper function doesn't invalidate the module's public interface,
so dependents don't recompile.

---

## 8. Self-Optimizing Runtime (SOR)

When compiled with `--sor`, the binary includes:

1. **Lightweight sampling profiler**: samples call stacks at 1KHz, adds ~0.1% overhead
2. **Hot function detector**: identifies functions executing >1% of total runtime
3. **Profile serializer**: writes profile data to a shared memory segment
4. **Background recompiler daemon** (`nvcd`): reads profiles, recompiles with PGO, hot-swaps

Hot-swap mechanism:
1. New optimized function compiled by `nvcd`
2. Original function patched with a jump to new version (atomic, lock-free)
3. No downtime, no restart, existing in-flight calls complete normally

After 60 seconds of runtime, Nuvola programs typically run 20-40% faster than their
initial compiled version.

---

## 9. Error Messages

Nuvola's compiler is famous for its error messages. Every error:
- Points to the exact problematic code
- Explains what went wrong in plain English
- Shows what the compiler expected
- Suggests the most likely fix
- Links to documentation

```
error[E0314]: cannot borrow `name` as mutable, as it is not declared as mutable
  → src/main.nvl:42:5
   |
38 |   name := "Alice"
   |   ---- help: make this binding mutable: `name =`
...
42 |   name = "Bob"
   |   ^^^^ cannot assign to an immutable binding
   |
   = note: immutable bindings cannot be reassigned after declaration
   = help: use `=` instead of `:=` to declare a mutable binding
   = see: https://docs.nuvola.dev/lang/bindings#mutability
```
