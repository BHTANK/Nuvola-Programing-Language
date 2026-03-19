# Nuvola Syntax Reference

## Quick-Reference Card

```
─────────────────────────────────────────────────────────────────────────────
BINDINGS
  x := expr           immutable binding (type inferred)
  x = expr            mutable binding (type inferred)
  x: Type = expr      typed binding
  x~ = expr           reactive binding
  (a, b) := tuple     tuple destructure
  { x, y } := s       struct destructure
  [h, ..t] := list    list destructure

FUNCTIONS
  fn name(a, b) => expr                  single-expression
  fn name(a: T, b: T) -> R              explicit types
  fn name(a, b)                         block body (last expr = return)
    ...
    result
  fn name(a: T = default)               default argument
  fn name(...xs: T) -> R                variadic
  fn(a, b) => expr                      anonymous lambda
  _ * 2                                 placeholder lambda (single arg)
  name(x)                               call
  name(x, y)                            curried: returns fn if partially applied
  name(x)!                              parallel call (returns Future)

TYPES
  i8 i16 i32 i64 i128 isize             signed integers
  u8 u16 u32 u64 u128 usize             unsigned integers
  f16 f32 f64 f128                      floats
  bool  char  str  String               text
  unit  never                           unit / bottom type
  (T, U, V)                             tuple
  [T; N]                                fixed array (stack)
  [T]                                   slice
  Vec(T) Map(K,V) Set(T)               dynamic collections
  Option(T)   T?                        optional
  Result(T,E) T!                        result / error
  Lazy(T)  Stream(T)  Signal(T)        temporal
  T~                                    reactive
  { field: T, ... }                     anonymous struct (row type)
  &T   &mut T                           borrow / mutable borrow
  Box(T)  Rc(T)  Arc(T)  Weak(T)       smart pointers

CONTROL FLOW
  if cond => a else b                   expression
  if cond                               statement form
    body
  else if cond
    body
  else
    body
  match expr                            pattern match
    pattern => expr
    pattern if guard => expr
  for x in iter                         for loop
  for i, x in iter.enumerate()
  while cond                            while loop
  loop                                  infinite loop
  break value                           exit loop with value
  continue                              next iteration
  return value                          explicit return

PATTERNS (in match / for / let)
  42                                    literal
  x                                     binding (captures value)
  _                                     wildcard (ignores value)
  (a, b, c)                             tuple destructure
  { x, y }                              struct destructure
  { x: alias }                          struct field with alias
  [h, ..t]                              list head + tail
  [a, b, ..]                            first two elements
  Some(x)   None                        Option
  Ok(x)     Err(e)                      Result
  TypeName(fields)                      enum variant
  TypeName { field }                    enum struct variant
  a | b                                 or-pattern
  x if guard                            guarded pattern
  ..val                                 spread (update syntax)
  is TypeName                           type test

OPERATORS
  +  -  *  /  %  **                     arithmetic
  //                                    integer division
  ==  !=  <  >  <=  >=                  comparison
  and  or  not                          logical (short-circuit)
  &  |  ^  ~  <<  >>  >>>               bitwise
  |>                                    pipe forward
  >>                                    function compose
  ?                                     error propagate
  !   (suffix)                          parallel execute
  ~   (suffix on binding)              reactive
  ??                                    nil/None coalescing
  ?.                                    optional chaining
  <=>                                   three-way compare
  ..  ..=                               range (exclusive / inclusive)
  @                                     address-of (unsafe)
  *                                     dereference (unsafe)

DECLARATIONS
  type Name                             struct type
    field: Type
    field: Type = default
  type Name(T, U, V)                    tuple struct
  type Alias = OtherType                type alias
  newtype Name(WrappedType)             newtype wrapper
  type Name                             enum type
    Variant
    Variant(field: T)
    Variant { field: T }
  trait Name                            trait definition
    fn method(self) -> T
    fn default_method(self) => ...
  impl TraitName for TypeName           trait implementation
  impl TypeName                         inherent methods

GENERICS
  fn name(x: T) where T: Trait         bounded generic
  fn name(x: T, y: T)                  same type inferred
  type Container(T)                     generic type
  type Pair(A, B)                       multi-param generic

MODULES
  import module                         import whole module
  import module.{fn, Type}              selective import
  import module as alias                aliased import
  import * from module                  glob import
  export fn name                        export from module
  export type Name                      export type

ATTRIBUTES
  @pure                                 no side effects
  @io                                   may do I/O
  @mut                                  may mutate external state
  @gpu                                  compile to GPU kernel
  @cpu                                  explicitly CPU (default)
  @comptime                             evaluate at compile time
  @macro                                define a macro
  @inline                               force inline
  @no_inline                            never inline
  @target(native, wasm, gpu)            compilation target
  @repr(C)                              C-compatible layout
  @packed                               no padding
  @align(N)                             force alignment to N bytes
  @derive(Trait1, Trait2)              auto-implement traits
  @test                                 test function
  @bench                                benchmark function
  @deprecated("reason")                deprecate a symbol
  @allow(warning_name)                  suppress a warning
  @unsafe                               unsafe block (raw pointers etc.)
  @ffi(lib)                             foreign function
  @mmio(addr)                           memory-mapped I/O register
  @interrupt                            interrupt handler
  @on(condition~)                       reactive trigger

CONCURRENCY
  expr!                                 parallel execute (Future)
  future.await                          wait for future
  await (a, b, c)                       wait for all
  race(a, b, c).await                   wait for first
  spawn => body                         spawn a task (in concurrent block)
  concurrent                            structured concurrency block
    task := spawn => ...
  async fn name                         async function
  await expr                            suspend until ready
  Channel(T, capacity: N)              typed channel
  ch.send(val)                          send to channel
  ch.recv()                             receive from channel
  select                                select first ready channel
    msg := ch1.recv() => handler
    msg := ch2.recv() => handler
  actor Name                            actor definition
    count: i64 = 0
    msg SomeMsg => ...
    msg Request -> T => ...

REACTIVE
  x~ = source                           reactive source
  y~ = x~ * 2                           reactive derived value
  @on(condition~)                       fires when condition becomes true
  source.stream()                       convert to Stream(T)
  Lazy => expensive()                   lazy evaluation

INTENT BLOCKS
  sort data by .field desc, .name asc
  find item in collection where condition
  group items by .field into summary
    count := count()
    total := sum(.amount)
  join a, b on a.id == b.fk into { ... }
  dedupe items by .key, keep .newest
  (a, b) := partition items by condition

MEMORY
  &x                                    immutable borrow
  &mut x                                mutable borrow
  x.clone()                             explicit deep copy
  Box.new(x)                            heap allocate
  Rc.new(x)                             reference count
  Arc.new(x)                            atomic reference count
  region name                           explicit memory region
    ...
  @unsafe                               unsafe block

ERROR HANDLING
  result?                               propagate error up
  result catch e => handler             handle error
  result catch SomeError => handler     type-matched handler
  result catch SomeError as e => ...    type-matched with binding
  Ok(value)  Err(error)                 construct Result
  Some(value)  None                     construct Option
─────────────────────────────────────────────────────────────────────────────
```

---

## Operator Precedence (highest to lowest)

```
1.  Unary: not  ~  -  &  &mut  *
2.  **  (right-associative)
3.  *  /  //  %
4.  +  -
5.  <<  >>  >>>
6.  &  (bitwise and)
7.  ^  (bitwise xor)
8.  |  (bitwise or)
9.  ..  ..=
10. ==  !=  <  >  <=  >=  <=>  is  has
11. and
12. or
13. ??  (nil coalescing)
14. |>  >>  (pipes — left-associative)
15. ?   (error propagation — postfix)
16. !   (parallel — postfix)
17. as  as!
18. =  :=  +=  -=  *=  /=  etc.
```

---

## Whitespace and Indentation

Nuvola uses **significant indentation** (like Python). The indentation character must be
consistent within a file (spaces recommended; 2 or 4 space indent width).

```nuvola
-- These are equivalent to braces in C-family languages:
fn foo()          -- opens a block
  statement_1     -- inside block
  statement_2     -- inside block
                  -- blank lines are fine inside blocks
  statement_3

-- Continuation: backslash or open bracket/paren continues to next line
result := very_long_function_name(
  argument_one,
  argument_two,
  argument_three,
)

-- OR use the pipeline which naturally spans lines:
result := data
  |> step_one
  |> step_two
  |> step_three
```

---

## Reserved Future Keywords

The following identifiers are reserved for future language versions:
```
yield  async_gen  type_of  size_of  align_of  offset_of
interface  class  extends  throws  checked  unchecked
atomic  volatile  restrict  extern  inline_asm
```
