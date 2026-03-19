# Nuvola Type System

## Overview

Nuvola's type system is the most expressive of any systems language. It unifies:
- **Hindley-Milner** type inference (global, complete)
- **Dependent types** (value-level constraints in types)
- **Refinement types** (logical predicates on values)
- **Linear types** (ownership / move semantics)
- **Effect types** (tracking I/O, mutation, nondeterminism)
- **Temporal types** (reactive, lazy, versioned, streaming)
- **Row polymorphism** (structural typing for records)

All of this resolves at **compile time**. There is zero runtime type overhead.

---

## 1. Inference

Nuvola uses bidirectional Hindley-Milner with constraint propagation.
Type annotations are optional and never required for correctness — only for documentation.

```nuvola
-- Full program, no annotations needed
fn process(data)
  parsed := data
    |> split("\n")
    |> filter(line => line.len > 0)
    |> map(line => line.trim().split(","))
    |> map(cols => { name: cols[0], value: cols[1].parse_f64() })

  stats := {
    count: parsed.len,
    total: parsed |> map(.value) |> sum,
    max:   parsed |> map(.value) |> max,
  }

  stats
```

The compiler infers: `fn process(data: str) -> { count: i64, total: f64, max: f64 }`
every type flows through without a single annotation.

---

## 2. Generics

Nuvola generics are **monomorphized** at compile time — each instantiation generates
specialized machine code. There is no boxing, no virtual dispatch, no overhead.

```nuvola
-- Generic function
fn first(xs: [T]) -> Option(T) where T: Copy
  if xs.len > 0 => Some(xs[0]) else None

-- Generic type
type Pair(A, B)
  left:  A
  right: B

fn swap(p: Pair(A, B)) -> Pair(B, A)
  Pair { left: p.right, right: p.left }

-- Generic trait
trait Container(T)
  fn insert(self, item: T)
  fn remove(self) -> Option(T)
  fn len(self) -> usize
  fn is_empty(self) -> bool => self.len() == 0   -- default
```

### 2.1 Trait Bounds

```nuvola
-- Single bound
fn print_it(x: T) where T: Display
  print("{x}")

-- Multiple bounds
fn sort_and_print(xs: [T]) where T: Ord + Display + Clone
  xs.sort()
  for x in xs => print("{x}")

-- Higher-kinded bounds (Functor, Monad, etc.)
fn map_twice(f: T -> U, g: U -> V, xs: F(T)) -> F(V) where F: Functor
  xs |> map(f) |> map(g)

-- Associated types
trait Iterator
  type Item
  fn next(self) -> Option(Self.Item)
  fn map(self, f: Self.Item -> B) -> MappedIterator(Self, B) => ...
```

---

## 3. Dependent Types

Dependent types encode value-level information into types, enabling the compiler
to prove properties that would otherwise only be checkable at runtime.

```nuvola
-- Length-indexed vector
type Vec(T, N: usize)
  data: [T; N]

-- Safe head: provably non-empty at compile time
fn head(v: Vec(T, N)) -> T where N > 0
  v.data[0]

-- Concatenation: result length is sum of inputs
fn concat(a: Vec(T, M), b: Vec(T, N)) -> Vec(T, M + N)
  Vec { data: [...a.data, ...b.data] }

-- Matrix multiplication: dimension compatibility proven at compile time
fn matmul(a: Matrix(f32, M, K), b: Matrix(f32, K, N)) -> Matrix(f32, M, N)
  a @ b

-- Bounds-checked index: provably in-bounds
fn safe_get(v: Vec(T, N), i: usize where i < N) -> T
  v.data[i]

-- Range type
type Port = u16 where self >= 1024           -- non-privileged ports only
type Probability = f64 where 0.0 <= self <= 1.0
type Byte = u8                               -- 0..=255 (trivially dependent)

-- Usage: constraint violation is a COMPILE ERROR
fn open_server(port: Port) => ...
open_server(80)    -- COMPILE ERROR: 80 < 1024, violates Port constraint
open_server(8080)  -- OK
```

---

## 4. Refinement Types

More expressive than dependent types for complex logical constraints:

```nuvola
-- Refinement on a struct
type SortedList(T) where T: Ord
  data: [T]
  @invariant(data |> windows(2) |> all(w => w[0] <= w[1]))

-- Functions on refined types
fn merge(a: SortedList(T), b: SortedList(T)) -> SortedList(T) where T: Ord
  -- Compiler verifies output satisfies SortedList invariant
  merge_sorted(a.data, b.data) |> SortedList

-- Email address refinement
type Email = str where
  self.contains("@") and
  self.split("@").len == 2 and
  self.split("@")[1].contains(".")

fn send_email(to: Email, body: str) => ...
send_email("not-an-email", "hi")   -- COMPILE ERROR: literal fails Email constraint
```

---

## 5. Effect Types

Functions carry an **effect signature** that the compiler tracks and enforces:

```nuvola
-- Effect annotations
@pure           -- no effects: no I/O, no mutation, always same output
@io             -- may perform I/O
@mut            -- may mutate shared state
@rand           -- uses random number generation (nondeterministic)
@net            -- specifically: network I/O
@fs             -- specifically: filesystem I/O
@time           -- reads the system clock
@panic          -- may panic (runtime failure)
@ffi            -- calls foreign code (unsafe effects)

-- Effects compose
@io @mut
fn write_and_log(data: str)
  log(data)
  File.write("output.txt", data)

-- Effect polymorphism: propagate caller's effects
fn twice(f: @E fn() -> T) -> T where E: Effects
  f(); f()

-- In test mode, all @io effects can be mocked
@test
fn test_fetch()
  mock.http.get("https://api.example.com/data") returns sample_data
  result := fetch_data()   -- uses mock, no actual network call
  assert result.len > 0
```

---

## 6. Temporal Types

### Reactive (`T~`)

```nuvola
-- Source
x~ = sensor.read_continuous()   -- updates as sensor fires

-- Derived (auto-recomputes)
y~ = x~ * 2 + 1

-- Type of y~: i64~ (reactive i64)
-- Compiler generates an update function: fn update_y() { y = x * 2 + 1 }
-- Called automatically when x changes
```

### Lazy (`Lazy(T)`)

```nuvola
-- Computed on first access, cached
expensive~ := Lazy => compute_pi(1_000_000)

-- First access triggers computation
x := expensive~.get()    -- computes now
y := expensive~.get()    -- returns cached value
```

### Versioned (`Versioned(T)`)

```nuvola
document = Versioned.new("initial content")
document.set("second version")
document.set("third version")

current := document.current         -- "third version"
prev    := document.at(-1)          -- "second version"
all     := document.history()       -- ["initial content", "second version", "third version"]
document.undo()                      -- rolls back to "second version"
```

### Stream (`Stream(T)`)

```nuvola
-- Infinite stream from a generator
naturals := Stream.generate(n = 0, => n + 1)
evens    := naturals |> filter(_ % 2 == 0)
first_10 := evens |> take(10) |> collect()   -- [0, 2, 4, 6, 8, 10, 12, 14, 16, 18]

-- From network / file
lines := File.stream("huge.log")
errors := lines |> filter(.contains("ERROR"))
errors |> take(100) |> for_each(print)
```

---

## 7. Row Polymorphism (Structural Typing)

Functions can accept any struct that has the required fields, without an explicit interface:

```nuvola
-- Accepts any type with a .name field of type str
fn greet(x: { name: str }) => "Hello, {x.name}!"

-- Accepts any type with .x and .y of type f64
fn distance_from_origin(p: { x: f64, y: f64 }) -> f64
  (p.x**2 + p.y**2).sqrt()

-- Works for all of these:
greet(Person { name: "Alice", age: 30 })          -- OK
greet(Dog { name: "Rex", breed: "Labrador" })      -- OK
greet(Company { name: "Acme", founded: 1920 })     -- OK

-- The row { name: str } is a lower bound — extra fields are fine
```

---

## 8. Type Aliases and Newtypes

```nuvola
-- Type alias (same type, just a name)
type Url = str
type UserId = u64
type Timestamp = i64

-- Newtype (different type, zero overhead)
newtype Meters(f64)
newtype Seconds(f64)
newtype MetersPerSecond(f64)

fn speed(d: Meters, t: Seconds) -> MetersPerSecond
  MetersPerSecond(d.0 / t.0)

-- Prevents unit confusion at compile time
d := Meters(100.0)
t := Seconds(9.58)
v := speed(d, t)               -- OK
v2 := speed(t, d)              -- COMPILE ERROR: wrong argument types
```

---

## 9. Error Types

```nuvola
-- Define error types as enums
type HttpError
  NotFound(url: str)
  Unauthorized
  RateLimit(retry_after: u32)
  Server(code: u16, body: str)
  Network(message: str)

-- Functions return Result
fn fetch(url: str) -> Result(Response, HttpError)
  ...

-- ? operator auto-propagates errors up the call stack
fn get_user(id: u64) -> Result(User, HttpError)
  response := fetch("https://api.example.com/users/{id}")?
  user := response.body |> parse_json(User)?
  Ok(user)

-- Typed error handling
match get_user(42)
  Ok(user)                       => print("Got user: {user.name}")
  Err(NotFound(url))             => print("Not found: {url}")
  Err(RateLimit(retry_after: t)) => sleep(t); retry()
  Err(Unauthorized)              => redirect_to_login()
  Err(e)                         => log_error(e)
```
