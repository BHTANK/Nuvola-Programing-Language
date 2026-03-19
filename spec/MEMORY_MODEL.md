# Nuvola Memory Model

## Philosophy

Memory management is the single greatest source of bugs and performance problems in software.
Nuvola eliminates both simultaneously:

- **No garbage collector** — no GC pauses, no GC pressure, no unpredictable latency
- **No manual memory management** — no malloc/free, no use-after-free, no leaks
- **No unsafe defaults** — null pointers cannot exist; buffer overflows are compile errors
- **Faster than C** — region-based allocation is faster than malloc for most programs

---

## 1. Stack Allocation (Default)

Small values with known lifetimes live on the stack. The compiler prefers stack allocation
wherever possible. The following always live on the stack:

```nuvola
x := 42               -- 8 bytes on stack
p := Point { x: 1.0, y: 2.0 }   -- 16 bytes on stack
arr := [1, 2, 3, 4]  -- [i64; 4] = 32 bytes on stack
```

The compiler's **escape analysis** determines whether a value "escapes" the current function.
If it does not escape, it stays on the stack — even if you use `&` references.

---

## 2. Region-Based Heap Allocation

When values must go on the heap, Nuvola uses **regions** (also called arenas or bump allocators).

A region is a contiguous block of memory. All allocations within a region use a simple
pointer bump — `ptr += size`. Freeing the entire region takes O(1) time: just reset the pointer.

```nuvola
-- The compiler infers regions automatically.
-- Every function creates an implicit region for its heap allocations.
fn build_report(data: [Row]) -> Report
  -- All strings, vecs, maps created here live in this function's region.
  -- When build_report returns, the region is freed in one instruction.
  lines := data |> map(row => format_row(row))
  header := "Report: {data.len} rows\n"
  body   := lines |> join("\n")
  Report { header, body }
  -- ^ Report is RETURNED, so it escapes. It's copied to caller's region.
  -- Everything else is freed here.
```

### Region Lifetime Rules

1. A value is allocated in the **current region** unless it must escape.
2. A value **escapes** if it is: returned, stored in a longer-lived structure, or sent to another thread.
3. Escaped values are **moved** to the caller's (or target's) region — one `memcpy`.
4. When a region goes out of scope, all its memory is freed in O(1).

### Explicit Regions

For long-lived programs, you can create explicit named regions:

```nuvola
-- Create a region that persists for the life of the request
fn handle_request(req: Request) -> Response
  region request_region
    -- All allocations in this block use request_region
    parsed  := parse(req.body)
    session := load_session(parsed.session_id)
    result  := process(parsed, session)
    Response.ok(result)
  -- request_region freed here — everything freed in one instruction

-- Persistent region (lives until explicitly freed)
cache_region := Region.new()
cache := cache_region.alloc(HashMap())
-- ... use cache across many requests ...
cache_region.free()   -- frees all cache memory at once
```

---

## 3. Ownership

Every value has exactly **one owner**. When the owner goes out of scope, the value is freed.
Values are **moved** by default when assigned or passed to functions.

```nuvola
-- Move semantics
a := Vec.of([1, 2, 3])
b := a              -- a MOVED into b
print(a)            -- COMPILE ERROR: a was moved
print(b)            -- OK

-- Explicit clone when you need two copies
c := b.clone()
print(b)            -- OK
print(c)            -- OK

-- Primitives implement Copy (no move needed)
x := 42
y := x              -- x is COPIED (it's a small integer)
print(x)            -- OK: x still valid
print(y)            -- OK
```

Types that implement `Copy` are always copied on assignment (they're small enough that this
is faster than tracking ownership). All primitive types are `Copy`. Structs can opt in:

```nuvola
@derive(Copy, Clone)
type Point { x: f64, y: f64 }   -- now Points are copied, not moved
```

---

## 4. Borrowing

A **borrow** is a temporary reference to a value. The borrowed value's owner retains ownership.
Borrows have lifetimes that the compiler tracks statically.

```nuvola
-- Immutable borrow
fn length(s: &str) -> usize => s.len()

name := "Alice"
n := length(&name)   -- borrow name, return length
print(name)          -- OK: borrow ended

-- Mutable borrow
fn push_one(v: &mut Vec(i64))
  v.push(1)

nums := Vec.new()
push_one(&mut nums)
print(nums)          -- [1]
```

### Borrow Rules (enforced at compile time)

1. At any point, a value has either:
   - **Any number of immutable borrows** (`&T`), OR
   - **Exactly one mutable borrow** (`&mut T`)
   - These are mutually exclusive.

2. **No borrow can outlive its owner.**

These two rules, enforced at compile time, eliminate:
- Data races (rule 1: no concurrent mutable access)
- Use-after-free (rule 2: borrows always valid)
- Iterator invalidation (a special case of rule 1/2)

---

## 5. Smart Pointers

### `Box(T)` — Single-owner heap value

```nuvola
large := Box.new(VeryLargeStruct { ... })   -- heap-allocated, single owner
-- Freed when large goes out of scope
```

### `Rc(T)` — Reference-counted, single-thread

```nuvola
shared := Rc.new(data)
copy_a := shared.clone()   -- increments refcount
copy_b := shared.clone()   -- increments refcount
-- Freed when all Rc handles drop
```

### `Arc(T)` — Atomic reference-counted, multi-thread

```nuvola
shared := Arc.new(data)
thread_copy := shared.clone()   -- safe to send to another thread
spawn => process(thread_copy)
```

### `Weak(T)` — Non-owning reference (breaks cycles)

```nuvola
parent := Arc.new(Node { children: [] })
child  := Arc.new(Node { parent: Weak.from(&parent) })
parent.children.push(child)
-- No cycle: child holds Weak ref to parent (doesn't prevent deallocation)
```

---

## 6. Null Safety

**Null pointers do not exist in Nuvola.** The `nil` keyword exists only as an `Option(T)` value
(`None`), not as a dangling pointer.

```nuvola
-- No nullable references. This is impossible in Nuvola:
x: &str = nil    -- COMPILE ERROR: references are always valid

-- Use Option for "maybe a value"
x: Option(str) = None
x: Option(str) = Some("hello")

-- Safe access with pattern matching
match user.middle_name
  Some(name) => print("Middle name: {name}")
  None       => print("No middle name")

-- Or with the ? shorthand
first_char := user.middle_name?.chars()?.next()   -- Option(char)
```

---

## 7. Memory Layout

Nuvola gives precise control over memory layout when needed:

```nuvola
-- Default layout (compiler optimizes field order for alignment)
type Point { x: f64, y: f64 }

-- Packed layout (no padding, may be unaligned)
@packed
type PackedHeader { magic: u32, version: u8, flags: u8 }

-- C-compatible layout (for FFI)
@repr(C)
type CPoint { x: f64, y: f64 }

-- Explicit alignment
@align(64)    -- cache-line aligned
type CacheLinePadded { data: [u8; 64] }

-- Explicit field ordering (for performance-critical structs)
@layout(x, y, z, w)
type Vec4 { x: f32, y: f32, z: f32, w: f32 }
```

---

## 8. SIMD and Vectorization

The compiler automatically vectorizes loops that operate on arrays of primitives.
Manual SIMD is available but rarely needed:

```nuvola
-- Auto-vectorized (compiler uses AVX-512 on supported hardware)
fn dot_product(a: [f32; N], b: [f32; N]) -> f32
  a |> zip(b) |> map((x, y) => x * y) |> sum

-- Manual SIMD when you need explicit control
import simd

fn dot_avx512(a: [f32; N], b: [f32; N]) -> f32
  result := simd.f32x16(0.0)
  for i in 0..N step 16
    va := simd.load_f32x16(a, i)
    vb := simd.load_f32x16(b, i)
    result += va * vb
  result |> simd.horizontal_sum
```

---

## 9. Memory Safety Guarantees

Nuvola provides a **formal proof** of memory safety: given well-typed Nuvola source code,
the compiled binary will never exhibit:

| Undefined Behavior | Status |
|---|---|
| Use-after-free | Impossible (borrow checker) |
| Double-free | Impossible (single ownership) |
| Buffer overflow | Impossible (bounds-checked by default) |
| Null pointer dereference | Impossible (no null pointers) |
| Data race | Impossible (borrow rules + Arc) |
| Stack overflow | Detected at compile time for static recursion; runtime guard for dynamic |
| Integer overflow | Detected in debug builds, wrapping in release (configurable) |
| Uninitialized reads | Impossible (all values must be initialized before use) |

In `@unsafe` blocks, some of these guarantees are relaxed — this is required for low-level
systems code and FFI. The compiler tracks `@unsafe` blocks and warns when they grow large.
