# Nuvola Language Specification
# Version 1.0 — Draft

---

## 1. Overview

Nuvola is a statically typed, compiled, garbage-collection-free language with automatic
parallelism, reactive primitives, intent-driven syntax, and a unified compilation model
targeting native code, WebAssembly, GPU kernels, and distributed compute.

The specification is organized as follows:
- §2  Lexical structure
- §3  Types
- §4  Expressions
- §5  Statements and declarations
- §6  Functions
- §7  Pattern matching
- §8  Concurrency and parallelism
- §9  Reactive values
- §10 Intent semantics
- §11 Memory model
- §12 Metaprogramming
- §13 Modules and packages
- §14 Interoperability
- §15 Compilation model

---

## 2. Lexical Structure

### 2.1 Comments

```
-- single line comment
--- triple-dash begins a doc comment (attached to next declaration)
--[[ multi-line comment ]]--
```

### 2.2 Identifiers

Identifiers follow `[a-zA-Z_][a-zA-Z0-9_]*`. Identifiers ending with `~` are reactive
(§9). Identifiers ending with `!` in expression position trigger parallel evaluation (§8.2).
Unicode identifiers are permitted in any script system.

Reserved words:
```
fn  type  trait  impl  match  if  else  for  while  loop  in  return
let  var  import  export  from  as  where  and  or  not  is  has
true  false  nil  self  super  async  await  spawn  send  recv
@gpu  @cpu  @comptime  @macro  @target  @inline  @pure  @unsafe
```

### 2.3 Literals

```
-- Integers
42          -- i64 (default)
42u         -- u64
42i32       -- i32
0xFF        -- hex
0b1010      -- binary
0o777       -- octal
1_000_000   -- underscores allowed

-- Floats
3.14        -- f64 (default)
3.14f32     -- f32
1.0e10      -- scientific
.5          -- leading dot OK

-- Strings
"hello"                     -- UTF-8 string
"hello {name}!"             -- interpolated
"""
  multi-line
  string
"""                         -- dedented multi-line
r"raw \n no escapes"        -- raw string
b"bytes\x00"                -- byte string

-- Characters
'a'         -- char (Unicode scalar)

-- Ranges
1..10       -- exclusive (1,2,...,9)
1..=10      -- inclusive (1,2,...,10)
..10        -- from start
10..        -- to end

-- Collections
[1, 2, 3]                   -- Array (fixed-size, stack)
{1, 2, 3}                   -- Set
{"key": "value"}            -- Map
(1, "hello", 3.14)          -- Tuple
```

### 2.4 Operators

```
-- Arithmetic
+  -  *  /  %  **          -- standard, ** is power
//                          -- integer division
+|  -|  *|                  -- wrapping arithmetic (no overflow panic)
+^  -^  *^                  -- saturating arithmetic

-- Comparison
==  !=  <  >  <=  >=        -- standard
<=>                         -- three-way comparison (-1, 0, 1)
~=                          -- fuzzy equality (within epsilon, for floats)
~~                          -- structural equality (deep compare)

-- Logic
and  or  not                -- boolean (short-circuit)
&&   ||   !                 -- bitwise equivalent
&    |    ^    ~            -- bitwise on integers
<<   >>   >>>               -- shift (>>> = unsigned right)

-- Pipeline
|>                          -- pipe: x |> f == f(x)
<|                          -- reverse pipe: f <| x == f(x)
>>                          -- function compose: f >> g == fn(x) => g(f(x))
<<                          -- reverse compose

-- Assignment
:=   =                      -- immutable / mutable bind
+=   -=   *=   /=   %=     -- compound assignment
|>=                         -- pipe-assign: x |>= f  ==  x = x |> f

-- Type
:                           -- type annotation
->                          -- function return type
=>                          -- single-expression function body / match arm
as                          -- type cast (checked)
as!                         -- type cast (unchecked, unsafe)

-- Memory
@                           -- address-of (in unsafe context)
*                           -- dereference (in unsafe context)

-- Special
?                           -- propagate error (like Rust ?)
!  (suffix)                 -- execute in parallel
~  (suffix on binding)      -- reactive value
..  (prefix on struct)      -- spread / update syntax
```

---

## 3. Types

### 3.1 Primitive Types

```
-- Integers (signed)
i8   i16   i32   i64   i128   isize

-- Integers (unsigned)
u8   u16   u32   u64   u128   usize

-- Floats
f16   f32   f64   f128

-- Other primitives
bool          -- true / false
char          -- Unicode scalar value (U+0000 to U+10FFFF)
str           -- UTF-8 string slice (fat pointer: ptr + len)
String        -- owned UTF-8 string (heap-allocated)
bytes         -- byte slice
Bytes         -- owned byte buffer
unit          -- () — zero-size "void" type
never         -- ! — type of diverging expressions (panic, infinite loop)
```

### 3.2 Compound Types

```
-- Tuples: anonymous, positional
(i32, str, f64)
()                          -- unit (empty tuple)

-- Arrays: fixed size, stack-allocated
[i32; 8]                    -- 8 i32s on the stack
[i32; N]                    -- N known at compile time

-- Slices: fat pointer into array or heap
[i32]                       -- slice of i32

-- Dynamic arrays: heap-allocated, resizable
Vec(T)                      -- equivalent to Rust Vec<T> / C++ vector<T>
Vec(i32)                    -- example

-- Maps and sets
Map(K, V)
Set(T)

-- Option: replaces null
Option(T)
  Some(T)
  None

-- Result: replaces exceptions
Result(T, E)
  Ok(T)
  Err(E)
```

### 3.3 User-Defined Types

**Structs** — named product types:
```nuvola
type Point
  x: f64
  y: f64

type Person
  name: str
  age: u8
  email: str = ""            -- default value

-- Tuple struct
type Rgb(u8, u8, u8)

-- Unit struct (zero size)
type Marker
```

**Enums** — named sum types:
```nuvola
type Shape
  Circle(radius: f64)
  Rectangle(w: f64, h: f64)
  Triangle(a: f64, b: f64, c: f64)
  Point                       -- unit variant

type Status
  Ok
  NotFound
  Error(code: i32, message: str)
```

**Traits** — interfaces / type classes:
```nuvola
trait Drawable
  fn draw(self) -> Canvas
  fn bounding_box(self) -> Rect
  fn area(self) -> f64 => 0.0    -- default implementation

trait Serialize
  fn to_json(self) -> str
  fn from_json(s: str) -> Self   -- associated function
```

**Implementations:**
```nuvola
impl Drawable for Circle
  fn draw(self) -> Canvas
    Canvas.circle(self.center, self.radius)

  fn area(self) -> f64 => PI * self.radius ** 2
```

### 3.4 Dependent Types

Nuvola's type system supports lightweight dependent types — types that carry value-level
constraints verified at compile time:

```nuvola
-- Constraint in type annotation
fn divide(a: f64, b: f64 where b != 0.0) -> f64 => a / b
fn head(xs: [T] where xs.len > 0) -> T => xs[0]

-- Named constraint types
type NonEmpty(T) = [T] where self.len > 0
type Positive    = i64  where self > 0
type InRange(lo, hi) = i64 where lo <= self <= hi

-- Dependent type in struct
type Matrix(T, rows: usize, cols: usize)
  data: [T; rows * cols]

fn dot(a: Matrix(f64, M, N), b: Matrix(f64, N, P)) -> Matrix(f64, M, P)
  -- dimension correctness is proven at compile time
```

### 3.5 Temporal Types

Nuvola introduces *temporal type qualifiers* that express how values change over time:

```
T           -- static: value does not change after binding
T~          -- reactive: value may update; dependents auto-recalculate
T?          -- optional (same as Option(T), syntactic sugar)
T!          -- Result(T, Error), auto-propagates errors
Lazy(T)     -- computed on first access, cached forever
Stream(T)   -- infinite or finite sequence of T values arriving over time
Signal(T)   -- like reactive but push-based (event model)
Versioned(T)-- every write creates a new version; full history accessible
```

### 3.6 Type Inference

Nuvola uses bidirectional type inference with global whole-program analysis.
In practice, you almost never write type annotations:

```nuvola
-- The compiler infers all of this:
data  := [1, 2, 3]               -- Vec(i64)
sum   := data |> fold(0, (+))    -- i64
avg   := sum.to_f64() / data.len -- f64
label := if avg > 2.0 => "high" else "low"  -- str
```

---

## 4. Expressions

### 4.1 If Expressions

`if` is an expression, always has a value:

```nuvola
x := if condition => value_a else value_b

-- Multi-arm
category := if score >= 90 => "A"
  else if score >= 80 => "B"
  else if score >= 70 => "C"
  else "F"

-- Without else: returns Option(T)
maybe := if ready => compute()    -- Option(str)
```

### 4.2 Block Expressions

Blocks are expressions; their value is the last expression inside:

```nuvola
result :=
  x := expensive_a()
  y := expensive_b()
  x + y                  -- block evaluates to x + y
```

### 4.3 Loop Expressions

```nuvola
-- while loop
while condition
  body

-- for loop (iterator protocol)
for item in collection
  process(item)

-- for with index
for i, item in collection.enumerate()
  print("{i}: {item}")

-- loop with result (break returns value)
found := loop
  item := next_item()
  if item.matches(target) => break item
  if done => break nil

-- Range loops
for i in 0..100
  ...
for i in 0..=100
  ...
```

### 4.4 Lambda / Anonymous Functions

```nuvola
add := fn(a, b) => a + b
greet := fn(name) =>
  msg := "Hello, {name}!"
  print(msg)
  msg

-- Short lambda syntax (underscore for single arg)
double := _ * 2
square := _ ** 2
is_even := _ % 2 == 0
```

### 4.5 Struct and Enum Construction

```nuvola
p := Point { x: 1.0, y: 2.0 }
p2 := Point { ..p, x: 5.0 }    -- update syntax, copies all fields from p, overrides x

s := Shape.Circle { radius: 3.0 }
s2 := Status.Error { code: 404, message: "Not found" }
```

### 4.6 Field Access and Method Calls

```nuvola
person.name          -- field access
person.greet()       -- method call
person.age |> add(1) -- pipe into function

-- Optional chaining
user?.profile?.avatar?.url    -- returns Option(str)

-- Null coalescing
name := user?.name ?? "Anonymous"
```

---

## 5. Declarations

### 5.1 Variable Declarations

```nuvola
-- Immutable (default)
x := 42
x = 43              -- ERROR: x is immutable

-- Mutable
y = 42
y = 43              -- OK

-- Typed
z: f64 = 3.14

-- Reactive (§9)
price~ = 100.0

-- Destructuring
(a, b, c) := tuple
{ x, y } := point
[first, ..rest] := list
[head, second, ..] := list    -- ignore tail after second
```

### 5.2 Function Declarations

```nuvola
-- Basic function
fn add(a: i64, b: i64) -> i64
  a + b

-- Inferred return type
fn greet(name) => "Hello, {name}!"

-- Generic function
fn max(a: T, b: T) -> T where T: Ord
  if a >= b => a else b

-- Multiple return values (via tuple)
fn min_max(xs: [i64]) -> (i64, i64)
  (xs |> min, xs |> max)

-- Named return values
fn stats(xs: [f64]) -> { mean: f64, std: f64 }
  mean := xs |> sum / xs.len
  variance := xs |> map(x => (x - mean) ** 2) |> sum / xs.len
  { mean, std: variance.sqrt() }

-- Variadic functions
fn sum_all(...nums: i64) -> i64
  nums |> fold(0, (+))
```

### 5.3 Type Declarations

See §3.3.

### 5.4 Trait Declarations

See §3.3.

### 5.5 Import Declarations

```nuvola
import math
import math.{sin, cos, PI}
import math as m
import * from math
import "https://pkg.nuvola.dev/http@2.1" as http    -- remote package
import "./local_module" as local
import "ffi/libssl.h" as ssl                         -- C FFI
```

---

## 6. Functions

### 6.1 Purity

Functions in Nuvola are **pure by default**. A pure function:
- Has no side effects (no I/O, no mutation of external state)
- Returns the same output for the same input always
- Can be automatically memoized, parallelized, and reordered by the compiler

```nuvola
@pure                          -- explicit annotation (redundant, shown for clarity)
fn distance(a: Point, b: Point) -> f64
  ((a.x - b.x)**2 + (a.y - b.y)**2).sqrt()
```

Functions that perform I/O or mutation must be marked `@io` or `@mut`:

```nuvola
@io
fn read_config(path: str) -> Config
  File.read(path) |> parse_config

@mut
fn increment(counter: &mut i64)
  *counter += 1
```

The compiler enforces this: calling an `@io` function from a `@pure` function is a compile error.

### 6.2 Currying and Partial Application

```nuvola
fn add(a, b) => a + b
add5 := add(5)          -- partial application: fn(b) => add(5, b)
result := add5(3)       -- 8

-- Pipe-friendly partial application
data |> map(multiply(2))    -- multiply(2) returns fn(x) => x * 2
data |> filter(greater_than(10))
```

### 6.3 Overloading

Functions can be overloaded by type:

```nuvola
fn area(c: Circle) -> f64 => PI * c.radius ** 2
fn area(r: Rectangle) -> f64 => r.w * r.h
fn area(t: Triangle) -> f64
  s := (t.a + t.b + t.c) / 2
  (s * (s - t.a) * (s - t.b) * (s - t.c)).sqrt()
```

Resolved at compile time — zero dispatch overhead.

### 6.4 Tail Call Optimization

All tail calls are guaranteed to be optimized. Mutual recursion is also TCO'd:

```nuvola
fn fib(n: u64, a: u64 = 0, b: u64 = 1) -> u64
  match n
    0 => a
    _ => fib(n - 1, b, a + b)   -- guaranteed TCO: O(1) stack
```

---

## 7. Pattern Matching

Pattern matching is the primary control flow mechanism in Nuvola. It is:
- **Exhaustive**: the compiler requires all cases to be covered
- **Zero-cost**: compiled to optimal jump tables or branch trees
- **Binding**: patterns can bind sub-values
- **Guard-aware**: patterns can have `if` guards

```nuvola
match value
  -- Literal patterns
  0         => "zero"
  1         => "one"
  2..=9     => "small"
  10..      => "large"
  -1        => "negative one"

  -- Type patterns
  is Circle => draw_circle(value)
  is Rect   => draw_rect(value)

  -- Destructuring patterns
  Point { x: 0, y }       => "on y-axis at {y}"
  Point { x, y: 0 }       => "on x-axis at {x}"
  Point { x, y }          => "at ({x}, {y})"

  -- Enum patterns
  Ok(val)                  => process(val)
  Err(NetworkError(code))  => retry_or_fail(code)
  Err(e)                   => log_error(e); default

  -- Tuple patterns
  (0, 0)                   => "origin"
  (x, 0)                   => "x-axis at {x}"
  (0, y)                   => "y-axis at {y}"
  (x, y)                   => "({x}, {y})"

  -- Guard patterns
  n if n % 2 == 0          => "even"
  n if n % 2 == 1          => "odd"

  -- OR patterns
  Red | Green | Blue       => "primary"

  -- Wildcard
  _                        => "anything else"
```

### 7.1 Destructuring in For Loops

```nuvola
for (key, value) in map.entries()
  print("{key} = {value}")

for { name, age } in people
  print("{name} is {age}")

for [first, ..rest] in batches
  process_head(first)
  process_rest(rest)
```

---

## 8. Concurrency and Parallelism

### 8.1 Automatic Parallelism

The Nuvola compiler performs **automatic parallelism extraction** (APX). Any expression that
the compiler can prove has no data dependencies on other concurrent expressions will be
scheduled for parallel execution automatically.

You do not need to think about threads, locks, or async/await in most cases.

```nuvola
-- These three calls have no dependencies on each other.
-- The compiler schedules them in parallel automatically.
a := fetch("https://api.example.com/users")
b := fetch("https://api.example.com/posts")
c := read_file("local_data.csv")

-- a, b, c are all guaranteed to be ready here.
process(a, b, c)
```

### 8.2 Explicit Parallel: `!` Operator

The `!` suffix on any expression forces parallel scheduling and immediately returns a future:

```nuvola
-- Process 10,000 images in parallel across all CPU cores
thumbnails := images |> map(resize(_, 128, 128))!

-- Wait for all:
done := thumbnails.await_all()

-- Or process as they complete:
for thumb in thumbnails.stream()
  save(thumb)
```

### 8.3 Structured Concurrency

Nuvola uses **structured concurrency**: every spawned task is owned by a scope.
Tasks cannot outlive the scope that spawned them. This eliminates entire classes of bugs.

```nuvola
concurrent
  task1 := spawn => fetch_users()
  task2 := spawn => fetch_products()
  task3 := spawn => fetch_orders()
  -- All tasks run in parallel; scope waits for all to finish
  -- If any task panics, all others are cancelled
-- task1, task2, task3 are fully resolved here

users, products, orders := (task1, task2, task3)
```

### 8.4 Channels

```nuvola
ch := Channel(i64, capacity: 100)

-- Producer
spawn =>
  for i in 0..1000
    ch.send(i)
  ch.close()

-- Consumer
for value in ch
  process(value)
```

### 8.5 Actors

```nuvola
actor Counter
  count: i64 = 0

  msg Increment => count += 1
  msg Decrement => count -= 1
  msg Get -> i64 => count
  msg Reset(to: i64) => count = to

-- Usage
c := Counter.spawn()
c.send(Increment)
c.send(Increment)
val := c.ask(Get)   -- synchronous request-response
```

### 8.6 GPU Execution

```nuvola
-- Any function decorated with @gpu compiles to a GPU kernel
-- No CUDA/Metal/OpenCL knowledge required

@gpu
fn matrix_multiply(a: Tensor[f32, M, K], b: Tensor[f32, K, N]) -> Tensor[f32, M, N]
  -- Compiler generates tiled GEMM with shared memory and optimal thread layout
  a @ b

@gpu
fn apply_relu(x: Tensor[f32, N]) -> Tensor[f32, N]
  x |> map(v => max(0.0, v))

-- Automatically moves data to GPU, executes, moves result back
-- Data that stays on GPU between calls is kept there (no unnecessary transfers)
result := matrix_multiply(a, b)
activated := apply_relu(result)
```

---

## 9. Reactive Values

A **reactive value** (declared with `~`) automatically propagates changes through a dependency
graph with zero polling overhead. The runtime maintains a directed acyclic dependency graph and
schedules recomputation using a topologically sorted update queue.

```nuvola
-- Source reactive values (updated externally)
mouse_x~ = 0.0
mouse_y~ = 0.0
window_width~ = 800.0

-- Derived reactive values (auto-recomputed when dependencies change)
normalized_x~ = mouse_x~ / window_width~    -- updates when either changes
distance~     = (mouse_x~**2 + mouse_y~**2).sqrt()

-- Reactive condition: fires a callback when it becomes true
@on(distance~ > 100.0)
fn on_far_from_origin()
  print("Mouse moved far: {distance~:.2f}px")

-- Reactive streams
key_presses~ = keyboard.stream()
words~        = key_presses~ |> buffer_until(key == ' ') |> map(join)

-- Reactive binding to external data source
db_users~     = db.watch("SELECT * FROM users ORDER BY created_at DESC LIMIT 100")
```

Reactive values compile to a **zero-overhead update graph** — no polling, no threads, no
callbacks. The compiler generates the optimal update order statically.

---

## 10. Intent Semantics

Intent blocks express *what* you want, not *how* to do it. The compiler selects the
optimal algorithm based on the data type, size hints, and hardware target.

```nuvola
-- SORT: compiler chooses pdqsort, radix, merge, or GPU sort
sort data by .score desc, .name asc

-- FIND: compiler generates indexed lookup if index exists, else linear scan
find user in users where user.id == target_id

-- GROUP: compiler chooses hash-group or sort-group based on cardinality
group orders by .region into totals
  total_revenue := sum(.price)
  order_count   := count()
  avg_order     := mean(.price)

-- JOIN: compiler picks hash-join, sort-merge, or nested-loop based on size
join users u, orders o on u.id == o.user_id
  into { user: u.name, order_total: o.total }

-- DEDUPLICATE: compiler picks hash-set or sort-dedup
dedupe emails by .address, keep .newest

-- PARTITION: split a collection based on predicate
(adults, minors) := partition users by .age >= 18
```

Intent blocks are **not syntactic sugar** for library calls. The compiler has deep semantic
understanding of these operations and can:
- Reorder them (if semantics allow)
- Push them into database queries (if the data source is a DB)
- Fuse multiple passes into one
- Vectorize with SIMD automatically
- Distribute across machines (if targeting distributed execution)

---

## 11. Memory Model

Nuvola is **garbage-collection free** with zero unsafe memory usage.
It achieves this through a combination of:

### 11.1 Region Inference

The compiler divides memory into **regions** — groups of objects with the same lifetime.
Regions are inferred automatically; you never declare them.

When a region's scope ends, all memory in it is freed in O(1) time regardless of
how many objects it contains (single `free()` call or bump-pointer reset).

```nuvola
-- All objects created in this scope are in the same region.
-- Freed atomically when the scope exits.
fn process_request(req: Request) -> Response
  parsed := parse_body(req.body)
  validated := validate(parsed)
  result := compute(validated)
  Response.ok(result)
  -- parsed, validated, result all freed here — one instruction
```

### 11.2 Ownership and Move Semantics

Each value has exactly one owner. Values are **moved** by default (not copied).
Copying requires explicit `.clone()` or the `Copy` trait.

```nuvola
a := String.from("hello")
b := a            -- a is MOVED into b. a is no longer valid.
print(a)          -- COMPILE ERROR: a was moved

c := b.clone()    -- explicit copy
print(b)          -- OK: b still valid
print(c)          -- OK: c is a new copy
```

### 11.3 Borrows

Values can be **borrowed** — temporary references that do not transfer ownership:

```nuvola
fn measure(s: &str) -> usize     -- immutable borrow
  s.len()

fn capitalize(s: &mut str)       -- mutable borrow
  s[0] = s[0].to_uppercase()

name := "alice"
len := measure(&name)            -- borrow, name still valid
capitalize(&mut name)            -- mutable borrow
```

The borrow checker runs at compile time. It guarantees:
- No use-after-free
- No double-free
- No data races
- No dangling references
- No null pointer dereferences

### 11.4 Reference Counting for Shared Ownership

When ownership must be shared (e.g., in graphs, caches), use `Rc` (single-threaded)
or `Arc` (thread-safe reference counting):

```nuvola
shared := Arc.new(expensive_data)
copy_a := shared.clone()   -- bumps refcount, O(1)
copy_b := shared.clone()   -- bumps refcount, O(1)
-- Freed when last Arc drops
```

Cycle detection is performed at compile time for statically known structures.
Runtime cycle breaking (weak references) is available for dynamic graphs.

---

## 12. Metaprogramming

### 12.1 Compile-Time Execution (`@comptime`)

Any expression annotated with `@comptime` is evaluated during compilation:

```nuvola
@comptime
routes := parse_yaml("routes.yaml")
  |> map(r => Route { path: r.path, handler: r.handler })

@comptime
SQL_CREATE_TABLE := """
  CREATE TABLE IF NOT EXISTS {TABLE_NAME} (
    id BIGSERIAL PRIMARY KEY,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
  )
""".format(TABLE_NAME: "users")
```

### 12.2 Macros

Macros receive and return AST nodes. They run at compile time and generate code:

```nuvola
@macro
fn derive_eq(type_def: TypeDef) -> Impl
  fields := type_def.fields
  Impl.new(type_def.name, "Eq",
    fn eq(self, other: Self) -> bool =>
      fields |> all(f => self[f] == other[f])
  )

@derive_eq
type Point { x: f64, y: f64 }
-- Compiler generates: impl Eq for Point { fn eq(self, other) => self.x == other.x and self.y == other.y }
```

### 12.3 Procedural Generation

```nuvola
-- Generate a typed SQL client from a database schema at compile time
@comptime db := Database.connect(env("DATABASE_URL"))
@comptime schema := db.schema()
@codegen typed_client := generate_typed_client(schema)

-- Now you have compile-time-checked SQL:
users := typed_client.users.find_all(where: .age > 18)
-- Compile error if .age doesn't exist or has wrong type
```

---

## 13. Modules and Packages

### 13.1 Modules

Each `.nvl` file is a module. The module name is the filename without extension.

```nuvola
-- math.nvl
export fn sin(x: f64) -> f64 => ...
export fn cos(x: f64) -> f64 => ...
export PI := 3.141592653589793

-- Internal (not exported)
fn _internal_helper() => ...
```

### 13.2 Packages

A directory with a `package.nvl` manifest is a package:

```nuvola
-- package.nvl
name    := "myapp"
version := "1.0.0"
authors := ["Your Name <you@example.com>"]

dependencies
  http   := "2.1"
  crypto := "1.4"
  db     := "3.0"

targets
  native := { optimize: "speed" }
  wasm   := { optimize: "size" }
```

### 13.3 Package Registry

Packages are hosted at `pkg.nuvola.dev`. Import by URL or short name:

```nuvola
import http                                         -- from registry
import "https://pkg.nuvola.dev/http@2.1" as http   -- pinned version
import "github.com/user/repo/module@abc123"         -- git source
```

---

## 14. Interoperability

### 14.1 C FFI

```nuvola
import "ffi/stdio.h" as c

@ffi(c)
fn printf(fmt: *c.char, ...) -> i32

fn main()
  printf("Hello from C: %d\n", 42)
```

### 14.2 Python Interop

```nuvola
import python as py

-- Call Python libraries directly
np     := py.import("numpy")
matrix := np.array([[1,2],[3,4]])
result := np.linalg.inv(matrix)
```

### 14.3 JavaScript / WASM Interop

When compiling to WASM, Nuvola can call JavaScript and expose functions to JS:

```nuvola
@wasm_import("env", "alert")
fn js_alert(msg: str)

@wasm_export
fn compute(x: f64) -> f64 => x * x + 1.0
```

---

## 15. Compilation Model

### 15.1 Compilation Stages

```
Source (.nvl)
  → Lexing / Parsing                   (~1ms per file)
  → Name resolution + type checking    (~5ms per file)
  → Whole-program type inference        (~10ms total)
  → Dependency analysis (APX)           (~5ms total)
  → Mid-level IR (Nuvola IR)            (~10ms total)
  → Optimization passes                 (~20ms total)
    - Dead code elimination
    - Inlining + specialization
    - Escape analysis → stack promotion
    - Loop unrolling + vectorization
    - Auto-parallelism scheduling
    - GPU kernel extraction
  → Backend codegen (LLVM / custom)     (~30ms total)
  → Link                                (~5ms total)
Total for a 50K LOC project: ~86ms
```

### 15.2 Incremental Compilation

Every compilation is incremental by default. Only changed modules and their dependents
are recompiled. A full `hello world` rebuild from cache takes 0ms (already compiled).

### 15.3 Compilation Targets

| Target flag    | Output                                      |
|----------------|---------------------------------------------|
| `native`       | Native binary for current OS/arch           |
| `native(arch)` | Cross-compile for specified arch            |
| `wasm`         | WebAssembly `.wasm` module                  |
| `wasm-js`      | WASM + JS glue code                         |
| `gpu-cuda`     | CUDA `.ptx` / `.cubin` kernel               |
| `gpu-metal`    | Apple Metal `.metallib`                     |
| `gpu-vulkan`   | SPIR-V for Vulkan compute                   |
| `embedded`     | Bare-metal RISC-V / ARM (no_std mode)       |
| `distributed`  | Distributed compute graph (runs on cluster) |
| `js`           | JavaScript (for environments without WASM)  |

### 15.4 Self-Optimizing Runtime (SOR)

When compiled with `--sor`, Nuvola programs include a lightweight profiling runtime
that collects hot-path data and submits it to a background recompilation daemon.
After 30 seconds of runtime, the binary is recompiled with profile-guided optimization
and hot-swapped in place — with zero downtime and zero restart.

This gives Nuvola programs the performance characteristics of JIT compilers while
maintaining the safety and predictability of AOT compilation.
