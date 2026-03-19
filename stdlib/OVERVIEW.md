# Nuvola Standard Library

## Overview

The Nuvola standard library (`nvl::std`) provides everything a programmer needs without
reaching for third-party packages. It is organized into focused modules:

---

## Module Index

| Module         | Description |
|----------------|-------------|
| `core`         | Primitives, Option, Result, basic traits |
| `collections`  | Vec, Map, Set, Deque, Heap, BloomFilter, SkipList |
| `string`       | UTF-8 strings, regex, parsing, formatting |
| `math`         | Arithmetic, linear algebra, statistics, FFT |
| `io`           | Files, streams, buffers |
| `net`          | HTTP, TCP, UDP, WebSockets, DNS, TLS |
| `db`           | SQL, NoSQL, connection pooling |
| `time`         | Dates, times, durations, timezones, scheduling |
| `fs`           | Filesystem operations |
| `process`      | Subprocesses, environment, signals |
| `crypto`       | Hashing, encryption, signing, TLS |
| `json`         | JSON parse/serialize (zero-copy) |
| `data`         | CSV, Parquet, Arrow, Avro, Protobuf |
| `ml`           | Tensors, autodiff, neural net layers, optimizers |
| `gpu`          | Low-level GPU control |
| `simd`         | Manual SIMD intrinsics |
| `os`           | OS-level primitives |
| `test`         | Testing, benchmarking, fuzzing, property testing |
| `async`        | Async primitives |
| `sync`         | Synchronization primitives |
| `log`          | Structured logging |
| `metrics`      | Counters, histograms, gauges |
| `trace`        | Distributed tracing (OpenTelemetry) |
| `plot`         | Data visualization |
| `geo`          | Geocoding, geospatial operations |
| `text`         | NLP: tokenization, embeddings, similarity |
| `img`          | Image decoding, encoding, manipulation |
| `audio`        | Audio decoding, DSP, synthesis |
| `video`        | Video decoding, transcoding |
| `cli`          | Command-line argument parsing |
| `config`       | Configuration (YAML, TOML, JSON, env) |
| `cache`        | In-process and distributed caching |
| `queue`        | Message queues (AMQP, Kafka, Redis) |
| `http`         | Full HTTP/1.1, HTTP/2, HTTP/3 client/server |
| `grpc`         | gRPC client and server |
| `graphql`      | GraphQL schema, resolvers, client |
| `auth`         | JWT, OAuth2, OIDC, session management |
| `email`        | SMTP, IMAP, template rendering |
| `pdf`          | PDF generation and parsing |
| `zip`          | Archive formats (zip, tar, gzip, zstd, lz4) |

---

## Selected API Examples

### `collections`

```nuvola
import collections.{Vec, Map, Set, Heap}

-- Vec
v = Vec.new()
v.push(1)
v.push(2)
v.extend([3, 4, 5])
v.sort()
v.dedup()
v |> filter(_ > 2) |> map(_ * 2) |> collect()

-- Map
m := { "a": 1, "b": 2, "c": 3 }
m.get("a")                    -- Some(1)
m.get_or("z", 0)              -- 0
m.entry("d").or_insert(4)     -- inserts 4 if "d" missing
m |> filter((k, v) => v > 1) |> collect()

-- Sorted set / priority queue
h := Heap(i64, order: .max)
h.push(5); h.push(1); h.push(3)
h.pop()   -- 5 (max first)
```

### `string`

```nuvola
import string.{Regex, Template}

s := "Hello, World!"
s.to_upper()          -- "HELLO, WORLD!"
s.split(", ")         -- ["Hello", "World!"]
s.contains("World")   -- true
s.replace("World", "Nuvola")
s.trim()
s.starts_with("Hello")
s.char_count()        -- 13 (Unicode-aware)

-- Regex
re := Regex.compile(r"(\w+)@(\w+)\.(\w+)")
match re.find(email)
  Some(m) => print("user={m[1]} domain={m[2]} tld={m[3]}")
  None    => print("no match")

emails := re.find_all(text) |> map(m => m.full_match) |> collect()

-- String formatting
fmt := Template("Hello, {name}! You have {count} messages.")
result := fmt.render({ name: "Alice", count: 3 })
```

### `net` / `http`

```nuvola
import http.{Client, Server, Router, Request, Response}

-- HTTP Client
client := Client.new()
  .timeout(30s)
  .retry(attempts: 3, backoff: exponential(base: 100ms))
  .user_agent("MyApp/1.0")

response := client.get("https://api.example.com/data").await
body := response.json(MyType).await

-- POST with JSON
result := client.post("https://api.example.com/users")
  .json({ name: "Alice", email: "alice@example.com" })
  .await
  .json(User)
  .await

-- HTTP Server (see examples/02_web_server.nvl for full example)
Server.new(router).port(8080).start()
```

### `db`

```nuvola
import db.{Pool, Transaction}

pool := Pool.connect(env("DATABASE_URL"), max: 20)

-- Query returning typed results
users := pool
  |> query("SELECT * FROM users WHERE age > $1 ORDER BY name", 18) as [User]

-- Transaction
pool.transaction(fn(tx) =>
  tx |> query("UPDATE accounts SET balance = balance - $1 WHERE id = $2", amount, from_id)
  tx |> query("UPDATE accounts SET balance = balance + $1 WHERE id = $2", amount, to_id)
)

-- Query builder
users := pool.table("users")
  .select("id", "name", "email")
  .where("age", ">", 18)
  .where("country", "=", "US")
  .order_by("name", .asc)
  .limit(100)
  .fetch() as [User]
```

### `crypto`

```nuvola
import crypto.{Hash, HMAC, AES, RSA, Ed25519, ChaCha20Poly1305}

-- Hashing
digest := Hash.sha256(data)
digest_hex := digest.to_hex()
Hash.sha3_256(data)
Hash.blake3(data)

-- HMAC
mac := HMAC.sha256(key: secret_key, data: message)
HMAC.verify(mac, key: secret_key, data: message)   -- constant-time compare

-- Symmetric encryption (authenticated)
key := ChaCha20Poly1305.generate_key()
nonce := ChaCha20Poly1305.random_nonce()
ciphertext := ChaCha20Poly1305.encrypt(plaintext, key, nonce, aad: "header")
plaintext2 := ChaCha20Poly1305.decrypt(ciphertext, key, nonce, aad: "header")

-- Asymmetric signatures
keypair := Ed25519.generate_keypair()
signature := Ed25519.sign(message, keypair.private)
Ed25519.verify(message, signature, keypair.public)   -- true/false

-- Password hashing (Argon2id — best practice)
hash := Password.hash(plaintext, algorithm: Argon2id)
Password.verify(plaintext, hash)   -- true/false
```

### `test`

```nuvola
import test.{it, describe, expect, bench, fuzz}

describe("String operations")
  it("reverses a string")
    expect("hello".reverse()).to_equal("olleh")

  it("handles unicode")
    expect("café".char_count()).to_equal(4)
    expect("café".byte_len()).to_equal(5)

  it("joins with separator")
    result := ["a", "b", "c"] |> join(", ")
    expect(result).to_equal("a, b, c")

describe("Math")
  it("square root of 2 is approximately 1.414")
    expect(2.0.sqrt()).to_be_close_to(1.41421356, within: 1e-7)

  it("division by zero panics")
    expect(=> 1 / 0).to_panic()

-- Benchmarks (run with: nvc bench)
bench("string reverse") => "hello world" * 1000 |> reverse()
bench("json parse")     => JSON.parse(large_json_str) as MyType

-- Property-based / fuzz testing
fuzz("sort is idempotent")
  data: Vec(i64) =>
    sorted := data.clone() |> sort()
    expect(sorted.clone() |> sort()).to_equal(sorted)

fuzz("encode + decode roundtrips")
  input: str =>
    expect(base64.decode(base64.encode(input))).to_equal(input)
```

### `ml`

```nuvola
import ml.{Tensor, Linear, Conv2D, LSTM, Attention, Adam, CrossEntropy}
import ml.{DataLoader, train_loop, evaluate}

-- Tensor operations (GPU-accelerated automatically)
a := Tensor.randn(128, 256)     -- [128, 256] float32 tensor
b := Tensor.randn(256, 512)
c := a @ b                       -- [128, 512] matrix multiply
d := c.relu()
e := d.softmax(dim: -1)

-- Automatic differentiation
x := Tensor.randn(32, 784).requires_grad()
loss := model.forward(x) |> cross_entropy(targets)
loss.backward()    -- compute gradients for all leaf tensors
optimizer.step()   -- update weights

-- Pre-built model architectures
transformer := Transformer {
  d_model:   512,
  n_heads:   8,
  n_layers:  6,
  d_ff:      2048,
  dropout:   0.1,
  vocab_size: 50257,
}

-- Training loop (one line)
train_loop(model: transformer, data: train_loader, optimizer: Adam(lr: 1e-4), epochs: 10)
```

### `log`

```nuvola
import log

-- Structured logging (JSON output in production, pretty in dev)
log.info("User logged in", { user_id: 42, ip: "1.2.3.4" })
log.warn("Slow query", { query: sql, duration_ms: 842 })
log.error("Payment failed", { order_id: "abc123", error: e.message })

-- Log levels: trace, debug, info, warn, error, fatal
log.set_level("warn")   -- only warn and above in production

-- Span-based tracing
span := log.span("process_request")
  .tag("user_id", user_id)
  .tag("endpoint", "/api/users")
  .start()

result := process()

span.finish(status: "ok")
```

---

## Standard Traits

The following traits are defined in `core` and implemented by built-in types:

| Trait | Methods | Implemented by |
|-------|---------|---------------|
| `Display` | `to_str(self) -> str` | all numeric, bool, str, Option, Result, collections |
| `Debug` | `debug_str(self) -> str` | everything (compiler can auto-derive) |
| `Eq` | `eq(self, other: Self) -> bool` | primitives, Option, Result, collections |
| `Ord` | `cmp(self, other: Self) -> Ordering` | all numeric, str, char |
| `Hash` | `hash(self, state: &mut Hasher)` | primitives, str, Option, Result, collections |
| `Copy` | (marker) | all primitive types |
| `Clone` | `clone(self) -> Self` | everything (can auto-derive) |
| `Default` | `default() -> Self` | 0 for numbers, "" for str, None for Option, etc. |
| `From(T)` | `from(t: T) -> Self` | numeric conversions, str from char, etc. |
| `Into(T)` | `into(self) -> T` | auto-derived from From |
| `Iterator` | `next(self) -> Option(Self.Item)` | Vec, Map, Set, Ranges, Streams |
| `Serialize` | `to_json/to_bytes` | can auto-derive for any struct/enum |
| `Deserialize` | `from_json/from_bytes` | can auto-derive for any struct/enum |
| `Component` | (ECS marker) | user-defined component types |
| `Drawable` | `draw(self, canvas: &Canvas)` | UI elements |
| `Send` | (marker, auto) | types safe to send to other threads |
| `Sync` | (marker, auto) | types safe to share across threads |
