# Nuvola Concurrency Model

## Design Principles

1. **Concurrency is automatic where possible** — the compiler extracts parallelism from sequential code
2. **Explicit where needed** — `!`, `spawn`, `concurrent`, `async` for manual control
3. **Structured** — no orphaned tasks; all concurrency has clear ownership and lifetime
4. **Data-race free** — type system makes data races impossible at compile time
5. **Composable** — all concurrency primitives compose cleanly with each other

---

## 1. Automatic Parallelism Extraction (APX)

The compiler's APX pass analyzes the **data dependency graph** of every function and
automatically schedules independent computations in parallel.

```nuvola
-- You write this sequential code:
fn analyze(dataset: [Row]) -> Report
  cleaned   := clean(dataset)          -- depends on dataset
  stats     := statistics(cleaned)     -- depends on cleaned
  outliers  := find_outliers(cleaned)  -- depends on cleaned
  viz_data  := visualize(stats)        -- depends on stats
  report    := generate(stats, outliers, viz_data)  -- depends on all

-- The compiler sees this dependency graph:
--   dataset
--     └── cleaned
--           ├── stats ──── viz_data ──┐
--           └── outliers ─────────────┴── report

-- And executes it as:
--   1. clean(dataset)                  [sequential]
--   2. statistics(cleaned)             [parallel]
--      find_outliers(cleaned)          [parallel]  ← runs at the same time as statistics
--   3. visualize(stats)                [after statistics]
--   4. generate(stats, outliers, viz_data)  [after all]
```

APX requires functions to be `@pure`. It also applies to pipeline stages:

```nuvola
-- Each stage in this pipeline runs as soon as its input is ready
-- Multiple stages run in parallel when data flow allows
result := raw_data
  |> parse          -- stage 1: starts immediately
  |> validate       -- stage 2: starts as soon as any parse output is ready
  |> enrich         -- stage 3: parallel with validate on different items
  |> store          -- stage 4
```

---

## 2. The `!` Parallel Operator

Appending `!` to any expression forces it to run in parallel, returning immediately
with a **future** (a handle to the pending result):

```nuvola
-- Fire and collect
a := compute_a()!    -- Future(ResultA)
b := compute_b()!    -- Future(ResultB), runs concurrently with a
c := compute_c()!    -- Future(ResultC), runs concurrently with a and b

-- Await individually
ra := a.await       -- blocks until a is done
rb := b.await
rc := c.await

-- Or await all at once
(ra, rb, rc) := await (a, b, c)    -- waits for all, returns in order

-- Or race — takes the first to finish
winner := race(a, b, c).await      -- whichever finishes first

-- Map over a collection in parallel
results := items |> map(process)!        -- all items processed concurrently
results_ordered := results.await_all()   -- Vec in original order
```

### Parallel HTTP Example

```nuvola
urls := [
  "https://api.service-a.com/data",
  "https://api.service-b.com/data",
  "https://api.service-c.com/data",
]

-- All three requests fire simultaneously
responses := urls |> map(fetch)!

-- Process each response as it arrives (not in order)
for response in responses.stream()
  process(response)

-- Or collect all (in original order)
all := responses.await_all()   -- waits for slowest
```

---

## 3. Structured Concurrency

The `concurrent` block spawns tasks that are **scoped** to the block.
The block does not complete until all tasks complete (or any task fails).

```nuvola
concurrent
  user_task    := spawn => fetch_user(user_id)
  orders_task  := spawn => fetch_orders(user_id)
  prefs_task   := spawn => fetch_preferences(user_id)
  -- All three run in parallel
  -- Block waits for all three
-- All tasks guaranteed complete here
user    := user_task.result
orders  := orders_task.result
prefs   := prefs_task.result
```

### Cancellation

If any task in a `concurrent` block panics, all other tasks are **cancelled**:

```nuvola
concurrent
  a := spawn => might_fail()      -- if this panics:
  b := spawn => long_computation() -- this is cancelled immediately
  -- No leaked resources, no orphaned threads
```

You can also cancel manually:

```nuvola
task := spawn => long_running_job()

-- Do other work...
if should_stop
  task.cancel()   -- sends cancellation signal
  task.await      -- waits for clean shutdown
```

---

## 4. Async / Await

For I/O-bound code, Nuvola uses cooperative async with an M:N runtime (M tasks on N OS threads):

```nuvola
-- Async function: suspends instead of blocking
async fn fetch_data(url: str) -> Result(str, HttpError)
  response := await http.get(url)    -- suspends here (no thread blocked)
  await response.text()

-- Calling async functions
async fn main()
  -- Sequential async calls
  a := await fetch_data("https://a.example.com")
  b := await fetch_data("https://b.example.com")

  -- Or concurrent
  (a, b) := await (
    fetch_data("https://a.example.com"),
    fetch_data("https://b.example.com"),
  )
```

### The Async Runtime

Nuvola's async runtime is:
- **Work-stealing**: idle threads steal tasks from busy threads' queues
- **Lock-free**: the task scheduler uses atomic operations only
- **Stack-switching**: async tasks have tiny initial stacks (4KB) that grow on demand
- **Integrated**: the same runtime handles CPU-bound parallelism and I/O-bound async

---

## 5. Channels

Channels are the primary communication mechanism between concurrent tasks.

```nuvola
-- Create typed, bounded channel
ch := Channel(T, capacity: N)

-- Unbounded (use carefully — can cause memory exhaustion)
ch := Channel(T, unbounded: true)

-- Send and receive
ch.send(value)          -- blocks if full
ch.try_send(value)      -- returns Err if full (non-blocking)
ch.send_timeout(value, 100ms)  -- blocks up to 100ms

value := ch.recv()      -- blocks if empty
value := ch.try_recv()  -- returns None if empty

-- Iterate (until channel closes)
for value in ch
  process(value)

-- Multiple channels: select the first ready
select
  msg1 := ch1.recv() => handle_a(msg1)
  msg2 := ch2.recv() => handle_b(msg2)
  _    := timeout(100ms) => handle_timeout()
```

### Pipeline Pattern

```nuvola
fn pipeline(source: [Item]) -> [Result]
  -- Stage channels
  raw      := Channel(Item, 100)
  parsed   := Channel(Parsed, 100)
  enriched := Channel(Enriched, 100)
  results  := Channel(Result, 100)

  -- Stage workers (each runs in parallel)
  spawn => for item in source    => raw.send(item); raw.close()
  spawn => for item in raw       => parsed.send(parse(item))
  spawn => for item in parsed    => enriched.send(enrich(item))
  spawn => for item in enriched  => results.send(transform(item))

  results |> collect()
```

---

## 6. Actors

The actor model provides isolated, message-passing concurrency:

```nuvola
actor DatabasePool
  connections: Vec(Connection) = []
  idle: Vec(Connection) = []

  msg Init(size: u32)
    for _ in 0..size
      conn := Connection.new(DB_URL)
      connections.push(conn)
      idle.push(conn)

  msg Acquire -> Connection
    if idle.is_empty()
      -- Wait for a connection to become available
      wait_for(idle.len > 0)
    idle.pop().unwrap()

  msg Release(conn: Connection)
    idle.push(conn)

-- Usage
pool := DatabasePool.spawn()
pool.send(Init(10))

conn := pool.ask(Acquire)      -- synchronous request
result := conn.query("SELECT 1")
pool.send(Release(conn))
```

### Actor Supervision

```nuvola
supervisor
  strategy: RestartOnFail      -- or: StopOnFail, EscalateOnFail

  child worker_a := WorkerA.spawn()
  child worker_b := WorkerB.spawn()
  -- If worker_a crashes, supervisor restarts it automatically
  -- worker_b continues running unaffected
```

---

## 7. Data Parallelism

Nuvola's data parallelism primitives are integrated with the type system:

```nuvola
-- Parallel map: splits work across CPU cores
results := data |> par_map(heavy_computation)

-- Parallel fold: parallel reduction (requires associative operation)
total := numbers |> par_fold(0, (+))

-- Parallel filter
valid := records |> par_filter(is_valid)

-- Parallel sort (uses parallel merge sort or sample sort)
sorted := large_array |> par_sort_by(.score)

-- Custom parallel work division
data |> par_chunks(1024) |> par_map(process_chunk) |> flatten
```

---

## 8. GPU Concurrency

GPU execution is expressed naturally:

```nuvola
@gpu
fn process_batch(inputs: Tensor[f32, N, D]) -> Tensor[f32, N, D]
  -- This runs as N parallel GPU threads, one per input
  inputs |> map(x => sigmoid(x @ weights + bias))

-- Data automatically moves to GPU, executes in parallel, returns to CPU
outputs := process_batch(input_batch)

-- Keep data on GPU between operations (no round-trips)
on_gpu
  a := load_tensor(path_a)    -- load directly to GPU memory
  b := load_tensor(path_b)
  c := matrix_multiply(a, b)  -- runs on GPU
  d := relu(c)                -- runs on GPU
  result := d.to_cpu()        -- one transfer back at the end
```

---

## 9. Distributed Concurrency

When compiled with `--target distributed`, the runtime can span multiple machines:

```nuvola
-- Distribute work across a cluster
cluster := Cluster.connect("cluster.example.com:7777")

results := huge_dataset
  |> cluster.distribute(nodes: 100)   -- splits data across 100 nodes
  |> par_map(analyze)                 -- runs in parallel on all nodes
  |> cluster.reduce(merge_results)    -- aggregates back to coordinator

-- Actor across machines — same syntax, different runtime
remote_worker := RemoteWorker.spawn(on: cluster.node("worker-1"))
remote_worker.send(Process(data))
result := remote_worker.ask(GetResult)
```

---

## 10. Synchronization Primitives

When you need low-level control (rare in well-designed Nuvola code):

```nuvola
-- Mutex
m := Mutex.new(data)
guard := m.lock()        -- blocks until acquired
guard.value += 1
-- Released when guard drops (end of scope)

-- Read-Write Lock
rw := RwLock.new(data)
r1 := rw.read()         -- multiple readers allowed simultaneously
r2 := rw.read()         -- this works
w  := rw.write()        -- BLOCKS: waits for all readers to finish

-- Atomic operations
counter := Atomic(i64, 0)
counter.fetch_add(1, ordering: SeqCst)
val := counter.load(ordering: Relaxed)

-- Semaphore
sem := Semaphore(permits: 10)
permit := sem.acquire()    -- blocks if 0 permits available
-- ... do work ...
sem.release(permit)

-- Barrier
barrier := Barrier(workers: 8)
-- In each of 8 workers:
barrier.wait()   -- all 8 block here until all have arrived
```

---

## 11. Memory Ordering

For lock-free data structures, Nuvola exposes memory ordering primitives:

```nuvola
type Ordering
  Relaxed      -- no ordering guarantees
  Acquire      -- no reads/writes can move before this load
  Release      -- no reads/writes can move after this store
  AcqRel       -- both Acquire and Release
  SeqCst       -- sequentially consistent (strongest, slowest)

-- Default for Atomic operations is SeqCst
-- Use Relaxed for performance-critical counters
hits := Atomic(u64, 0)
hits.fetch_add(1, ordering: Relaxed)   -- fastest possible increment
```
