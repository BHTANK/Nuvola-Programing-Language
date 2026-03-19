/// nuvola.h — Stage-0 Bootstrap Runtime
///
/// Single-header C library.  Every generated .c file begins with:
///   #include "nuvola.h"
///
/// Value model: NvVal is a tagged union (16 bytes on 64-bit).
///   NIL, INT, FLOAT, BOOL  — stored inline (no heap)
///   STR                    — heap-allocated null-terminated UTF-8
///   LIST                   — heap-allocated dynamic array of NvVal
///   MAP                    — heap-allocated array of NvEntry (linear scan)
///   FN                     — function pointer + optional closure env

#pragma once

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <math.h>
#include <stdarg.h>
#include <ctype.h>
#include <pthread.h>

// ─────────────────────────────────────────────────────────────────────────────
// Optional Boehm GC  (compile with -DNUVOLA_GC -lgc)
// When enabled: malloc/free route through GC_malloc/GC_free, preventing leaks
// in long-running servers and recursive programs without explicit memory mgmt.
// ─────────────────────────────────────────────────────────────────────────────
#ifdef NUVOLA_GC
#  include <gc.h>
#  define NV_MALLOC(n)    GC_malloc(n)
#  define NV_REALLOC(p,n) GC_realloc(p, n)
#  define NV_FREE(p)      ((void)(p))   /* GC collects — explicit free is no-op */
#  define NV_STRDUP(s)    GC_strdup(s)
#  define NV_GC_INIT()    GC_init()
#else
#  define NV_MALLOC(n)    malloc(n)
#  define NV_REALLOC(p,n) realloc(p, n)
#  define NV_FREE(p)      free(p)
#  define NV_STRDUP(s)    strdup(s)
#  define NV_GC_INIT()    ((void)0)
#endif

// ─────────────────────────────────────────────────────────────────────────────
// Runtime call stack  (M18: stack traces)
//
// NV_ENTER("fn_name") — placed at the start of every generated function.
// Uses __attribute__((cleanup)) so the frame pops automatically on any return,
// even early returns and longjmp unwinds (GC, try/catch, etc.).
// nv_print_trace() — prints the call chain; called before every fatal exit.
// ─────────────────────────────────────────────────────────────────────────────
typedef struct { const char *fn; } _NvFrame;
static _NvFrame _nv_stack[256];
static int      _nv_sdepth = 0;

static void _nv_pop_frame(int *_u) { (void)_u; if (_nv_sdepth > 0) _nv_sdepth--; }

// NV_ENTER(name): push a frame; the cleanup attribute pops it on scope exit.
#define NV_ENTER(name) \
    if (_nv_sdepth < 255) _nv_stack[_nv_sdepth++].fn = (name); \
    int _nv_fguard __attribute__((cleanup(_nv_pop_frame))) = 0

static inline void nv_print_trace(void) {
    if (_nv_sdepth == 0) return;
    fprintf(stderr, "Stack trace (most recent call last):\n");
    for (int _i = 0; _i < _nv_sdepth; _i++)
        fprintf(stderr, "  in %s\n", _nv_stack[_i].fn);
}

// _NV_FATAL_EXIT(): print trace then exit(1) — used by all runtime error paths.
#define _NV_FATAL_EXIT() do { nv_print_trace(); exit(1); } while(0)

// ─────────────────────────────────────────────────────────────────────────────
// Per-request bump arena (used by HTTP server to prevent per-request leaks)
// Thread-local; only active between nv_arena_begin() / nv_arena_end() calls.
// When active, nv_str() allocates strings from the arena instead of heap.
// After nv_arena_end(), the arena offset resets — all strings freed at once.
// ─────────────────────────────────────────────────────────────────────────────
#define NV_ARENA_SIZE (4 * 1024 * 1024)  // 4 MB per-request arena

typedef struct {
    char  *buf;
    size_t used;
    size_t cap;
} NvArena;

static __thread NvArena _nv_arena        = {NULL, 0, 0};
static __thread int     _nv_arena_active = 0;

static inline void nv_arena_begin(void) {
    if (!_nv_arena.buf) {
        _nv_arena.buf = (char*)NV_MALLOC(NV_ARENA_SIZE);
        _nv_arena.cap = NV_ARENA_SIZE;
    }
    _nv_arena.used   = 0;
    _nv_arena_active = 1;
}

static inline void nv_arena_end(void) {
    _nv_arena_active = 0;
    _nv_arena.used   = 0;
    // Note: buf is kept alive for the next request (reuse the 4MB block)
}

// Allocate a copy of s from the arena (if active) or fall back to heap strdup
static inline char *_nv_strdup(const char *s) {
    if (!s) s = "";
    size_t n = strlen(s) + 1;
    if (_nv_arena_active && _nv_arena.buf) {
        // Align to 8 bytes
        size_t aligned = (n + 7) & ~(size_t)7;
        if (_nv_arena.used + aligned <= _nv_arena.cap) {
            char *p = _nv_arena.buf + _nv_arena.used;
            memcpy(p, s, n);
            _nv_arena.used += aligned;
            return p;
        }
        // Arena full — fall through to heap
    }
    return NV_STRDUP(s);
}

// ─────────────────────────────────────────────────────────────────────────────
// Hot-path inlining — must be defined early so all numeric/call helpers use it
// ─────────────────────────────────────────────────────────────────────────────
#define NV_HOT __attribute__((always_inline)) static inline

// ─────────────────────────────────────────────────────────────────────────────
// Value types
// ─────────────────────────────────────────────────────────────────────────────

typedef enum { NV_NIL=0, NV_INT, NV_FLOAT, NV_BOOL, NV_STR, NV_LIST, NV_MAP, NV_FN, NV_PTR, NV_CHAN } NvTag;

typedef struct NvVal  NvVal;
typedef struct NvList NvList;
typedef struct NvMap  NvMap;
typedef struct NvChan NvChan;
typedef NvVal (*NvFn)(NvVal, NvVal);           // fn(arg, env)
typedef NvVal (*NvFn2)(NvVal, NvVal, NvVal);   // fn(a, b, env)
typedef NvVal (*NvFn3)(NvVal, NvVal, NvVal, NvVal); // fn(a,b,c,env)

// NvClosure: heap-allocated fn+env pair.
// Storing a pointer in NvVal keeps the union at 8 bytes,
// making sizeof(NvVal) = 16 (fits in 2 registers on x86-64).
typedef struct NvClosure { NvFn fn; void *env; } NvClosure;

// NvList forward: data is pointer so only forward decl needed here
struct NvList { NvVal *data; size_t len, cap; };

// NvVal must be fully defined before NvEntry (NvEntry holds NvVal by value)
// sizeof(NvVal) == 16: tag(4)+pad(4)+union(8).  Fits in two 64-bit registers,
// so function return no longer requires a hidden stack pointer — critical for
// recursive numeric code (e.g. fib).
struct NvVal {
    NvTag tag;
    union {
        int64_t    i;     // INT, BOOL (0/1)
        double     f;     // FLOAT
        char      *s;     // STR  (heap, null-terminated)
        NvList    *list;  // LIST (heap)
        NvMap     *map;   // MAP  (heap)
        NvClosure *clo;   // FN   (heap — keeps union at 8 bytes → struct at 16)
        void      *p;     // PTR  (raw C pointer — FFI use only)
        NvChan    *chan;   // CHAN (heap, pthread-backed channel)
    };
};

// NvEntry and NvMap defined after NvVal is complete
typedef struct { NvVal key; NvVal val; } NvEntry;
struct NvMap  { NvEntry *data; size_t len, cap; };

// ─────────────────────────────────────────────────────────────────────────────
// Constructors
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_nil(void)         { NvVal v={0}; v.tag=NV_NIL;   return v; }
static inline NvVal nv_int(int64_t i)    { NvVal v={0}; v.tag=NV_INT;   v.i=i;    return v; }
static inline NvVal nv_float(double f)   { NvVal v={0}; v.tag=NV_FLOAT; v.f=f;    return v; }
static inline NvVal nv_bool(int b)       { NvVal v={0}; v.tag=NV_BOOL;  v.i=(b?1:0); return v; }
static inline NvVal nv_fn(NvFn fn) {
    NvVal v={0}; v.tag=NV_FN;
    NvClosure *c = (NvClosure*)NV_MALLOC(sizeof(NvClosure));
    c->fn=fn; c->env=NULL; v.clo=c; return v;
}
static inline NvVal nv_closure(NvFn fn, NvVal env) {
    NvVal v={0}; v.tag=NV_FN;
    NvClosure *c = (NvClosure*)NV_MALLOC(sizeof(NvClosure));
    NvVal *ep = (NvVal*)NV_MALLOC(sizeof(NvVal)); *ep = env;
    c->fn=fn; c->env=ep; v.clo=c; return v;
}
NV_HOT NvVal nv_call_1(NvVal f, NvVal a1) {
    NvVal env = f.clo->env ? *(NvVal*)f.clo->env : nv_nil();
    return f.clo->fn(a1, env);
}
NV_HOT NvVal nv_call_2(NvVal f, NvVal a1, NvVal a2) {
    NvVal env = f.clo->env ? *(NvVal*)f.clo->env : nv_nil();
    return ((NvFn2)f.clo->fn)(a1, a2, env);
}
NV_HOT NvVal nv_call_3(NvVal f, NvVal a1, NvVal a2, NvVal a3) {
    NvVal env = f.clo->env ? *(NvVal*)f.clo->env : nv_nil();
    return ((NvFn3)f.clo->fn)(a1, a2, a3, env);
}

static inline NvVal nv_str(const char *s) {
    NvVal v={0}; v.tag=NV_STR;
    v.s = _nv_strdup(s ? s : "");
    return v;
}

// ─────────────────────────────────────────────────────────────────────────────
// List operations
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_list_new(void) {
    NvVal v={0}; v.tag=NV_LIST;
    v.list = (NvList*)NV_MALLOC(sizeof(NvList));
    v.list->data = NULL; v.list->len = 0; v.list->cap = 0;
    return v;
}

static inline void nv_list_push_mut(NvVal *lst, NvVal item) {
    NvList *l = lst->list;
    if (l->len >= l->cap) {
        l->cap = l->cap ? l->cap * 2 : 8;
        l->data = (NvVal*)NV_REALLOC(l->data, l->cap * sizeof(NvVal));
    }
    l->data[l->len++] = item;
}

static inline NvVal nv_list_of(size_t n, ...) {
    NvVal lst = nv_list_new();
    va_list ap; va_start(ap, n);
    for (size_t i = 0; i < n; i++)
        nv_list_push_mut(&lst, va_arg(ap, NvVal));
    va_end(ap);
    return lst;
}

NV_HOT NvVal _nv_list_get(NvVal lst, int64_t idx, const char *f, int ln) {
    NvList *l = lst.list;
    if (idx < 0) idx = (int64_t)l->len + idx;
    if (idx < 0 || (size_t)idx >= l->len) {
        fprintf(stderr, "%s:%d: list index %lld out of bounds (len=%zu)\n",
                f, ln, (long long)idx, l->len);
        _NV_FATAL_EXIT();
    }
    return l->data[idx];
}
#define nv_list_get(lst, idx) _nv_list_get(lst, idx, __FILE__, __LINE__)

NV_HOT void _nv_list_set(NvVal lst, int64_t idx, NvVal val, const char *f, int ln) {
    NvList *l = lst.list;
    if (idx < 0) idx = (int64_t)l->len + idx;
    if (idx < 0 || (size_t)idx >= l->len) {
        fprintf(stderr, "%s:%d: list assignment index %lld out of bounds (len=%zu)\n",
                f, ln, (long long)idx, l->len);
        _NV_FATAL_EXIT();
    }
    l->data[idx] = val;
}
#define nv_list_set(lst, idx, val) _nv_list_set(lst, idx, val, __FILE__, __LINE__)

// ─────────────────────────────────────────────────────────────────────────────
// Map operations
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_map_new(void) {
    NvVal v={0}; v.tag=NV_MAP;
    v.map = (NvMap*)NV_MALLOC(sizeof(NvMap));
    v.map->data = NULL; v.map->len = 0; v.map->cap = 0;
    return v;
}

// Forward declarations for cross-dependencies
static inline int   nv_val_eq(NvVal a, NvVal b);
static inline NvVal nv_to_str(NvVal v);

static inline void nv_map_set(NvVal *m, NvVal key, NvVal val) {
    NvMap *mp = m->map;
    // Update existing key
    for (size_t i = 0; i < mp->len; i++) {
        if (nv_val_eq(mp->data[i].key, key)) { mp->data[i].val = val; return; }
    }
    // Insert new
    if (mp->len >= mp->cap) {
        mp->cap = mp->cap ? mp->cap * 2 : 8;
        mp->data = (NvEntry*)NV_REALLOC(mp->data, mp->cap * sizeof(NvEntry));
    }
    mp->data[mp->len++] = (NvEntry){key, val};
}

static inline NvVal _nv_map_get(NvVal m, NvVal key, const char *f, int ln) {
    NvMap *mp = m.map;
    for (size_t i = 0; i < mp->len; i++)
        if (nv_val_eq(mp->data[i].key, key)) return mp->data[i].val;
    { NvVal _ks = nv_to_str(key);
      fprintf(stderr, "%s:%d: key '%s' not found in map\n", f, ln, _ks.s); _NV_FATAL_EXIT(); }
    return nv_nil(); // unreachable
}
#define nv_map_get(m, key) _nv_map_get(m, key, __FILE__, __LINE__)
// Returns nil for missing keys (used by [] indexing on maps)
static inline NvVal nv_map_get_opt(NvVal m, NvVal key) {
    if (m.tag != NV_MAP) return nv_nil();
    NvMap *mp = m.map;
    for (size_t i = 0; i < mp->len; i++)
        if (nv_val_eq(mp->data[i].key, key)) return mp->data[i].val;
    return nv_nil();
}

static inline NvVal nv_map_of(size_t n, ...) {
    NvVal m = nv_map_new();
    va_list ap; va_start(ap, n);
    for (size_t i = 0; i < n; i++) {
        NvVal k = va_arg(ap, NvVal);
        NvVal v = va_arg(ap, NvVal);
        nv_map_set(&m, k, v);
    }
    va_end(ap);
    return m;
}

// ─────────────────────────────────────────────────────────────────────────────
// Truthiness and equality
// ─────────────────────────────────────────────────────────────────────────────

NV_HOT int nv_truthy(NvVal v) {
    switch (v.tag) {
        case NV_NIL:   return 0;
        case NV_BOOL:  return v.i != 0;
        case NV_INT:   return v.i != 0;
        case NV_FLOAT: return v.f != 0.0;
        case NV_STR:   return v.s && v.s[0] != '\0';
        case NV_LIST:  return v.list->len > 0;
        case NV_MAP:   return v.map->len  > 0;
        default:       return 1;
    }
}

static inline int nv_val_eq(NvVal a, NvVal b) {
    if (a.tag != b.tag) {
        // INT/FLOAT coercion
        if (a.tag == NV_INT && b.tag == NV_FLOAT)  return (double)a.i == b.f;
        if (a.tag == NV_FLOAT && b.tag == NV_INT)  return a.f == (double)b.i;
        return 0;
    }
    switch (a.tag) {
        case NV_NIL:   return 1;
        case NV_INT:   case NV_BOOL: return a.i == b.i;
        case NV_FLOAT: return a.f == b.f;
        case NV_STR:   return strcmp(a.s, b.s) == 0;
        case NV_LIST: {
            if (a.list->len != b.list->len) return 0;
            for (size_t i = 0; i < a.list->len; i++)
                if (!nv_val_eq(a.list->data[i], b.list->data[i])) return 0;
            return 1;
        }
        default: return a.i == b.i;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Arithmetic
// ─────────────────────────────────────────────────────────────────────────────

NV_HOT double nv_to_f(NvVal v) {
    return v.tag == NV_FLOAT ? v.f : (double)v.i;
}
NV_HOT int64_t nv_to_i(NvVal v) {
    return v.tag == NV_INT ? v.i : (int64_t)v.f;
}

// Forward declare for nv_add and tensor dispatch
static inline NvVal nv_list_concat(NvVal a, NvVal b);
// Forward declarations for tensor binary ops (defined in nuvola_tensor.h)
NvVal nv_tensor_add(NvVal a, NvVal b);
NvVal nv_tensor_sub(NvVal a, NvVal b);
NvVal nv_tensor_mul(NvVal a, NvVal b);
NvVal nv_tensor_div(NvVal a, NvVal b);

NV_HOT NvVal nv_add(NvVal a, NvVal b) {
    if (a.tag == NV_MAP || b.tag == NV_MAP) return nv_tensor_add(a, b);
    if (a.tag == NV_LIST && b.tag == NV_LIST) return nv_list_concat(a, b);
    if (a.tag == NV_STR || b.tag == NV_STR) {
        // String concatenation
        NvVal sa = (a.tag == NV_STR) ? a : nv_to_str(a);
        NvVal sb = (b.tag == NV_STR) ? b : nv_to_str(b);
        size_t la = strlen(sa.s), lb = strlen(sb.s);
        char *buf = (char*)NV_MALLOC(la + lb + 1);
        memcpy(buf, sa.s, la); memcpy(buf + la, sb.s, lb); buf[la+lb] = '\0';
        NvVal r = nv_str(buf); NV_FREE(buf); return r;
    }
    if (a.tag == NV_FLOAT || b.tag == NV_FLOAT) return nv_float(nv_to_f(a) + nv_to_f(b));
    return nv_int(a.i + b.i);
}
NV_HOT NvVal nv_sub(NvVal a, NvVal b) {
    if (a.tag == NV_MAP || b.tag == NV_MAP) return nv_tensor_sub(a, b);
    if (a.tag == NV_FLOAT || b.tag == NV_FLOAT) return nv_float(nv_to_f(a) - nv_to_f(b));
    return nv_int(a.i - b.i);
}
NV_HOT NvVal nv_mul(NvVal a, NvVal b) {
    if (a.tag == NV_MAP || b.tag == NV_MAP) return nv_tensor_mul(a, b);
    if (a.tag == NV_FLOAT || b.tag == NV_FLOAT) return nv_float(nv_to_f(a) * nv_to_f(b));
    return nv_int(a.i * b.i);
}
static inline NvVal _nv_div(NvVal a, NvVal b, const char *f, int ln) {
    if (a.tag == NV_MAP || b.tag == NV_MAP) return nv_tensor_div(a, b);
    /* int / int → integer division (truncate toward zero), matching Python3 // and C behavior */
    if ((a.tag == NV_INT || a.tag == NV_BOOL) && (b.tag == NV_INT || b.tag == NV_BOOL)) {
        if (b.i == 0) { fprintf(stderr, "%s:%d: integer division by zero\n", f, ln); _NV_FATAL_EXIT(); }
        return nv_int(a.i / b.i);
    }
    double bv = nv_to_f(b);
    if (bv == 0.0) { fprintf(stderr, "%s:%d: float division by zero\n", f, ln); _NV_FATAL_EXIT(); }
    return nv_float(nv_to_f(a) / bv);
}
#define nv_div(a, b) _nv_div(a, b, __FILE__, __LINE__)

static inline NvVal _nv_idiv(NvVal a, NvVal b, const char *f, int ln) {
    if (b.i == 0) { fprintf(stderr, "%s:%d: integer division by zero\n", f, ln); _NV_FATAL_EXIT(); }
    int64_t q = a.i / b.i;
    if ((a.i ^ b.i) < 0 && q * b.i != a.i) q--;
    return nv_int(q);
}
#define nv_idiv(a, b) _nv_idiv(a, b, __FILE__, __LINE__)

static inline NvVal _nv_mod(NvVal a, NvVal b, const char *f, int ln) {
    if (b.i == 0) { fprintf(stderr, "%s:%d: modulo by zero\n", f, ln); _NV_FATAL_EXIT(); }
    int64_t r = a.i % b.i;
    if (r != 0 && (r ^ b.i) < 0) r += b.i;
    return nv_int(r);
}
#define nv_mod(a, b) _nv_mod(a, b, __FILE__, __LINE__)
static inline NvVal nv_pow(NvVal a, NvVal b) {
    if (a.tag == NV_INT && b.tag == NV_INT && b.i >= 0) {
        int64_t result = 1, base = a.i, exp = b.i;
        while (exp > 0) { if (exp & 1) result *= base; base *= base; exp >>= 1; }
        return nv_int(result);
    }
    return nv_float(pow(nv_to_f(a), nv_to_f(b)));
}
static inline NvVal nv_matmul(NvVal a, NvVal b) { (void)a; (void)b; return nv_nil(); }
NV_HOT NvVal nv_neg(NvVal a) {
    return a.tag == NV_FLOAT ? nv_float(-a.f) : nv_int(-a.i);
}

// ─────────────────────────────────────────────────────────────────────────────
// Comparisons
// ─────────────────────────────────────────────────────────────────────────────

NV_HOT NvVal nv_eq(NvVal a, NvVal b) { return nv_bool(nv_val_eq(a,b)); }
NV_HOT NvVal nv_ne(NvVal a, NvVal b) { return nv_bool(!nv_val_eq(a,b)); }
NV_HOT NvVal nv_lt(NvVal a, NvVal b) {
    if (a.tag == NV_STR && b.tag == NV_STR) return nv_bool(strcmp(a.s, b.s) < 0);
    return nv_bool(nv_to_f(a) < nv_to_f(b));
}
NV_HOT NvVal nv_le(NvVal a, NvVal b) {
    if (a.tag == NV_STR && b.tag == NV_STR) return nv_bool(strcmp(a.s, b.s) <= 0);
    return nv_bool(nv_to_f(a) <= nv_to_f(b));
}
NV_HOT NvVal nv_gt(NvVal a, NvVal b) {
    if (a.tag == NV_STR && b.tag == NV_STR) return nv_bool(strcmp(a.s, b.s) > 0);
    return nv_bool(nv_to_f(a) > nv_to_f(b));
}
NV_HOT NvVal nv_ge(NvVal a, NvVal b) {
    if (a.tag == NV_STR && b.tag == NV_STR) return nv_bool(strcmp(a.s, b.s) >= 0);
    return nv_bool(nv_to_f(a) >= nv_to_f(b));
}
static inline NvVal nv_and(NvVal a, NvVal b) { return nv_bool(nv_truthy(a) && nv_truthy(b)); }
static inline NvVal nv_or (NvVal a, NvVal b) { return nv_bool(nv_truthy(a) || nv_truthy(b)); }
static inline NvVal nv_not(NvVal a)           { return nv_bool(!nv_truthy(a)); }
static inline NvVal nv_is (NvVal a, NvVal b)  { return nv_eq(a, b); }

// ─────────────────────────────────────────────────────────────────────────────
// String conversion
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_to_str(NvVal v) {
    char buf[256];
    switch (v.tag) {
        case NV_NIL:   return nv_str("nil");
        case NV_BOOL:  return nv_str(v.i ? "true" : "false");
        case NV_INT:   snprintf(buf, sizeof(buf), "%lld", (long long)v.i);  return nv_str(buf);
        case NV_FLOAT: {
            // Remove trailing zeros for clean output
            snprintf(buf, sizeof(buf), "%g", v.f);
            return nv_str(buf);
        }
        case NV_STR:   return nv_str(v.s);
        default:       return nv_str("<value>");
    }
}

// type(v) → "int" | "float" | "str" | "bool" | "nil" | "list" | "map" | "fn"
static inline NvVal nv_type_of(NvVal v) {
    switch (v.tag) {
        case NV_NIL:   return nv_str("nil");
        case NV_INT:   return nv_str("int");
        case NV_FLOAT: return nv_str("float");
        case NV_BOOL:  return nv_str("bool");
        case NV_STR:   return nv_str("str");
        case NV_LIST:  return nv_str("list");
        case NV_MAP:   return nv_str("map");
        case NV_FN:    return nv_str("fn");
        default:       return nv_str("unknown");
    }
}

// Convert with format spec (e.g. ".2f")
static inline NvVal nv_to_str_fmt(NvVal v, const char *fmt) {
    char buf[256];
    if (fmt && fmt[0] == '.') {
        // Precision format: .Nf
        char cfmt[32];
        snprintf(cfmt, sizeof(cfmt), "%s", fmt);
        // Replace leading '.' with '%' prefix → e.g. ".2f" → "%.2f"
        char full[32];
        snprintf(full, sizeof(full), "%%%s", fmt);
        snprintf(buf, sizeof(buf), full, nv_to_f(v));
        return nv_str(buf);
    }
    return nv_to_str(v);
}

// Concatenate N string values
static inline NvVal nv_str_concat_n(int n, ...) {
    // First pass: total length
    va_list ap;
    char **parts = (char**)NV_MALLOC(n * sizeof(char*));
    NvVal *vals = (NvVal*)NV_MALLOC(n * sizeof(NvVal));
    va_start(ap, n);
    size_t total = 0;
    for (int i = 0; i < n; i++) {
        vals[i] = va_arg(ap, NvVal);
        NvVal s = nv_to_str(vals[i]);
        parts[i] = NV_STRDUP(s.s);
        total += strlen(parts[i]);
    }
    va_end(ap);
    char *out = (char*)NV_MALLOC(total + 1); out[0] = '\0';
    for (int i = 0; i < n; i++) { strcat(out, parts[i]); NV_FREE(parts[i]); }
    NvVal result = nv_str(out);
    NV_FREE(out); NV_FREE(parts); NV_FREE(vals);
    return result;
}

// ─────────────────────────────────────────────────────────────────────────────
// String methods
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_str_trim(NvVal s) {
    const char *p = s.s;
    while (*p && isspace((unsigned char)*p)) p++;
    const char *e = p + strlen(p);
    while (e > p && isspace((unsigned char)*(e-1))) e--;
    char *out = (char*)NV_MALLOC(e - p + 1);
    memcpy(out, p, e - p); out[e - p] = '\0';
    NvVal r = nv_str(out); NV_FREE(out); return r;
}

static inline NvVal nv_str_upper(NvVal s) {
    char *out = NV_STRDUP(s.s);
    for (char *p = out; *p; p++) *p = (char)toupper((unsigned char)*p);
    NvVal r = nv_str(out); NV_FREE(out); return r;
}

static inline NvVal nv_str_lower(NvVal s) {
    char *out = NV_STRDUP(s.s);
    for (char *p = out; *p; p++) *p = (char)tolower((unsigned char)*p);
    NvVal r = nv_str(out); NV_FREE(out); return r;
}

static inline NvVal nv_str_contains(NvVal s, NvVal sub) {
    if (s.tag != NV_STR || sub.tag != NV_STR) return nv_bool(0);
    return nv_bool(strstr(s.s, sub.s) != NULL);
}

static inline NvVal nv_str_starts_with(NvVal s, NvVal prefix) {
    return nv_bool(strncmp(s.s, prefix.s, strlen(prefix.s)) == 0);
}

static inline NvVal nv_str_ends_with(NvVal s, NvVal suffix) {
    size_t sl = strlen(s.s), el = strlen(suffix.s);
    if (el > sl) return nv_bool(0);
    return nv_bool(strcmp(s.s + sl - el, suffix.s) == 0);
}

static inline NvVal nv_str_replace(NvVal s, NvVal from, NvVal to) {
    // Simple single-occurrence replace
    char *p = strstr(s.s, from.s);
    if (!p) return nv_str(s.s);
    size_t pre = p - s.s;
    size_t tlen = strlen(to.s), flen = strlen(from.s), slen = strlen(s.s);
    char *out = (char*)NV_MALLOC(slen - flen + tlen + 1);
    memcpy(out, s.s, pre);
    memcpy(out + pre, to.s, tlen);
    strcpy(out + pre + tlen, p + flen);
    NvVal r = nv_str(out); NV_FREE(out); return r;
}

static inline NvVal nv_str_split(NvVal s, NvVal delim) {
    NvVal list = nv_list_new();
    const char *d = delim.s; size_t dl = strlen(d);
    const char *p = s.s, *q;
    while ((q = strstr(p, d)) != NULL) {
        char *tok = (char*)NV_MALLOC(q - p + 1);
        memcpy(tok, p, q - p); tok[q - p] = '\0';
        nv_list_push_mut(&list, nv_str(tok)); NV_FREE(tok);
        p = q + dl;
    }
    nv_list_push_mut(&list, nv_str(p));
    return list;
}

static inline NvVal nv_str_join(NvVal sep, NvVal list) {
    NvList *l = list.list;
    if (l->len == 0) return nv_str("");
    size_t total = 0;
    size_t dl = strlen(sep.s);
    for (size_t i = 0; i < l->len; i++) {
        NvVal sv = nv_to_str(l->data[i]);
        total += strlen(sv.s);
        if (i + 1 < l->len) total += dl;
    }
    char *out = (char*)NV_MALLOC(total + 1); out[0] = '\0';
    for (size_t i = 0; i < l->len; i++) {
        NvVal sv = nv_to_str(l->data[i]);
        strcat(out, sv.s);
        if (i + 1 < l->len) strcat(out, sep.s);
    }
    NvVal r = nv_str(out); NV_FREE(out); return r;
}

static inline NvVal nv_str_len(NvVal s) {
    return nv_int((int64_t)strlen(s.s));
}

// ─────────────────────────────────────────────────────────────────────────────
// .len field (polymorphic — works on str, list, map)
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_len(NvVal v) {
    switch (v.tag) {
        case NV_STR:  return nv_int((int64_t)strlen(v.s));
        case NV_LIST: return nv_int((int64_t)v.list->len);
        case NV_MAP:  return nv_int((int64_t)v.map->len);
        default:      return nv_int(0);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Index operator (polymorphic — list, map, string)
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal _nv_index(NvVal obj, NvVal idx, const char *f, int ln) {
    switch (obj.tag) {
        case NV_LIST: {
            int64_t i64 = (idx.tag == NV_INT || idx.tag == NV_BOOL) ? idx.i : (int64_t)idx.f;
            return _nv_list_get(obj, i64, f, ln);
        }
        case NV_MAP: {
            // Tensor integer indexing: t[i] → element (1D) or row list (2D)
            if (idx.tag == NV_INT) {
                NvVal data_key = nv_str("data");
                NvVal data = nv_map_get_opt(obj, data_key);
                if (data.tag == NV_LIST) {
                    // Check shape for 2D indexing
                    NvVal shape = nv_map_get_opt(obj, nv_str("shape"));
                    if (shape.tag == NV_LIST && shape.list->len >= 2) {
                        int64_t cols = nv_list_get(shape, 1).i;
                        int64_t row  = idx.i;
                        // Return a list slice for row i
                        NvVal row_list = nv_list_new();
                        for (int64_t c = 0; c < cols; c++)
                            nv_list_push_mut(&row_list, nv_list_get(data, row * cols + c));
                        return row_list;
                    }
                    // 1D: return element directly
                    return nv_list_get(data, idx.i);
                }
            }
            return nv_map_get_opt(obj, idx); // nil for missing key
        }
        case NV_STR: {
            int64_t i = idx.i;
            size_t len = strlen(obj.s);
            if (i < 0) i = (int64_t)len + i;
            if (i < 0 || (size_t)i >= len) {
                fprintf(stderr, "%s:%d: string index out of bounds\n", f, ln); _NV_FATAL_EXIT();
            }
            char buf[2] = { obj.s[i], '\0' };
            return nv_str(buf);
        }
        default:
            fprintf(stderr, "%s:%d: [] on non-indexable value (tag=%d)\n", f, ln, obj.tag);
            _NV_FATAL_EXIT();
    }
}
#define nv_index(obj, idx) _nv_index(obj, idx, __FILE__, __LINE__)

// ─────────────────────────────────────────────────────────────────────────────
// C FFI helpers  (unbox NvVal → raw C type; box raw C type → NvVal)
// Used by extern fn call sites generated by the compiler.
// ─────────────────────────────────────────────────────────────────────────────

// Constructors
static inline NvVal nv_ptr(void *p)      { NvVal v={0}; v.tag=NV_PTR; v.p=p; return v; }

// Unbox: NvVal → raw C value
static inline int64_t    nv_as_i64(NvVal v)  { return v.tag==NV_FLOAT?(int64_t)v.f:v.i; }
static inline double     nv_as_f64(NvVal v)  { return v.tag==NV_FLOAT?v.f:(double)v.i; }
static inline float      nv_as_f32(NvVal v)  { return (float)nv_as_f64(v); }
static inline const char*nv_to_cstr(NvVal v) { return v.tag==NV_STR&&v.s?v.s:""; }
static inline void*      nv_to_ptr(NvVal v)  {
    if (v.tag==NV_PTR)  return v.p;
    if (v.tag==NV_INT)  return (void*)(intptr_t)v.i;
    return NULL;
}

// ─────────────────────────────────────────────────────────────────────────────
// Number conversions
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_to_int(NvVal v) {
    switch (v.tag) {
        case NV_INT:   return v;
        case NV_FLOAT: return nv_int((int64_t)v.f);
        case NV_BOOL:  return nv_int(v.i);
        case NV_STR:   return nv_int(atoll(v.s));
        default:       return nv_int(0);
    }
}

static inline NvVal nv_to_float(NvVal v) {
    return nv_float(nv_to_f(v));
}

// ─────────────────────────────────────────────────────────────────────────────
// I/O builtins
// ─────────────────────────────────────────────────────────────────────────────

static inline void nv_print_val(NvVal v) {
    switch (v.tag) {
        case NV_NIL:   printf("nil"); break;
        case NV_BOOL:  printf("%s", v.i ? "true" : "false"); break;
        case NV_INT:   printf("%lld", (long long)v.i); break;
        case NV_FLOAT: printf("%g", v.f); break;
        case NV_STR:   printf("%s", v.s); break;
        case NV_LIST: {
            printf("[");
            for (size_t i = 0; i < v.list->len; i++) {
                if (i) printf(", ");
                if (v.list->data[i].tag == NV_STR)
                    printf("\"%s\"", v.list->data[i].s);
                else
                    nv_print_val(v.list->data[i]);
            }
            printf("]");
            break;
        }
        case NV_MAP: {
            printf("{");
            for (size_t i = 0; i < v.map->len; i++) {
                if (i) printf(", ");
                if (v.map->data[i].key.tag == NV_STR) printf("\"%s\"", v.map->data[i].key.s);
                else nv_print_val(v.map->data[i].key);
                printf(": ");
                if (v.map->data[i].val.tag == NV_STR) printf("\"%s\"", v.map->data[i].val.s);
                else nv_print_val(v.map->data[i].val);
            }
            printf("}");
            break;
        }
        default: printf("<fn>"); break;
    }
}

static inline NvVal nv_print(NvVal v) {
    nv_print_val(v); printf("\n"); fflush(stdout); return nv_nil();
}

// print without newline
static inline NvVal nv_print_no_nl(NvVal v) {
    nv_print_val(v); fflush(stdout); return nv_nil();
}

// eprint / eprintln — stderr
static inline NvVal nv_eprint(NvVal v) {
    nv_print_val(v); fprintf(stderr, "\n"); return nv_nil();
}

// read_file(path) -> str  (returns nil on error)
static inline NvVal nv_read_file(NvVal path) {
    if (path.tag != NV_STR) return nv_nil();
    FILE *f = fopen(path.s, "r");
    if (!f) return nv_nil();
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    rewind(f);
    char *buf = (char*)NV_MALLOC(sz + 1);
    if (!buf) { fclose(f); return nv_nil(); }
    size_t n = fread(buf, 1, sz, f);
    buf[n] = '\0';
    fclose(f);
    NvVal r = nv_str(buf);
    NV_FREE(buf);
    return r;
}

// write_file(path, content) -> nil
static inline NvVal nv_write_file(NvVal path, NvVal content) {
    if (path.tag != NV_STR || content.tag != NV_STR) return nv_nil();
    FILE *f = fopen(path.s, "w");
    if (!f) return nv_nil();
    fputs(content.s, f);
    fclose(f);
    return nv_nil();
}

// path_dirname(path) -> directory part (e.g. "/foo/bar.nvl" -> "/foo", "bar.nvl" -> ".")
static inline NvVal nv_path_dirname(NvVal path) {
    if (path.tag != NV_STR) return nv_str(".");
    const char *p = path.s;
    const char *last = strrchr(p, '/');
    if (!last) return nv_str(".");
    if (last == p) return nv_str("/");
    size_t len = (size_t)(last - p);
    char *buf = (char*)NV_MALLOC(len + 1);
    memcpy(buf, p, len);
    buf[len] = '\0';
    NvVal r = nv_str(buf);
    NV_FREE(buf);
    return r;
}

// path_join(dir, rel) -> joined path
static inline NvVal nv_path_join(NvVal dir, NvVal rel) {
    if (dir.tag != NV_STR || rel.tag != NV_STR) return nv_nil();
    // If rel is absolute, return it directly
    if (rel.s[0] == '/') return rel;
    size_t dlen = strlen(dir.s);
    size_t rlen = strlen(rel.s);
    // Strip trailing slash from dir
    while (dlen > 1 && dir.s[dlen-1] == '/') dlen--;
    char *buf = (char*)NV_MALLOC(dlen + 1 + rlen + 1);
    memcpy(buf, dir.s, dlen);
    buf[dlen] = '/';
    memcpy(buf + dlen + 1, rel.s, rlen);
    buf[dlen + 1 + rlen] = '\0';
    NvVal r = nv_str(buf);
    NV_FREE(buf);
    return r;
}

// Global argv/argc storage for args() builtin
static int   _nv_argc = 0;
static char **_nv_argv = NULL;

// args() -> list of strings
static inline NvVal nv_args(void) {
    NvVal r = nv_list_new();
    for (int i = 0; i < _nv_argc; i++)
        nv_list_push_mut(&r, nv_str(_nv_argv[i]));
    return r;
}

// nv_assert_ is a macro so __FILE__/__LINE__ resolve to the .nvl source when
// the generated C file contains #line directives (emitted by the compiler).
static inline NvVal _nv_assert_impl(NvVal cond, NvVal msg,
                                     const char *file, int line) {
    if (!nv_truthy(cond)) {
        fprintf(stderr, "%s:%d: assertion failed: %s\n",
                file, line, msg.tag == NV_STR ? msg.s : "<msg>");
        _NV_FATAL_EXIT();
    }
    return nv_nil();
}
#define nv_assert_(cond, msg) _nv_assert_impl(cond, msg, __FILE__, __LINE__)

// nv_panic — fatal runtime error with source location
#define nv_panic(fmt, ...) \
    do { fprintf(stderr, "%s:%d: " fmt "\n", __FILE__, __LINE__, ##__VA_ARGS__); _NV_FATAL_EXIT(); } while(0)

// ─────────────────────────────────────────────────────────────────────────────
// Functional builtins: map, filter, sum, range
// ─────────────────────────────────────────────────────────────────────────────

// nv_map_fn: map(fn, list)
static inline NvVal nv_map_fn(NvVal fn_val, NvVal lst) {
    NvVal result = nv_list_new();
    NvList *l = lst.list;
    for (size_t i = 0; i < l->len; i++)
        nv_list_push_mut(&result, nv_call_1(fn_val, l->data[i]));
    return result;
}

static inline NvVal nv_filter(NvVal fn_val, NvVal lst) {
    NvVal result = nv_list_new();
    NvList *l = lst.list;
    for (size_t i = 0; i < l->len; i++)
        if (nv_truthy(nv_call_1(fn_val, l->data[i])))
            nv_list_push_mut(&result, l->data[i]);
    return result;
}

static inline NvVal nv_sum(NvVal lst) {
    NvList *l = lst.list;
    if (l->len == 0) return nv_int(0);
    NvVal acc = l->data[0];
    for (size_t i = 1; i < l->len; i++) acc = nv_add(acc, l->data[i]);
    return acc;
}

static inline NvVal nv_range(NvVal start, NvVal end_, int inclusive) {
    NvVal lst = nv_list_new();
    int64_t s = start.i, e = end_.i;
    if (inclusive) { for (int64_t i = s; i <= e; i++) nv_list_push_mut(&lst, nv_int(i)); }
    else           { for (int64_t i = s; i <  e; i++) nv_list_push_mut(&lst, nv_int(i)); }
    return lst;
}

static inline NvVal nv_max_fn(NvVal lst) {
    NvList *l = lst.list;
    if (l->len == 0) return nv_nil();
    NvVal m = l->data[0];
    for (size_t i = 1; i < l->len; i++)
        if (nv_truthy(nv_gt(l->data[i], m))) m = l->data[i];
    return m;
}

static inline NvVal nv_min_fn(NvVal lst) {
    NvList *l = lst.list;
    if (l->len == 0) return nv_nil();
    NvVal m = l->data[0];
    for (size_t i = 1; i < l->len; i++)
        if (nv_truthy(nv_lt(l->data[i], m))) m = l->data[i];
    return m;
}

static inline NvVal nv_sorted(NvVal lst) {
    NvList *l = lst.list;
    NvVal result = nv_list_new();
    for (size_t i = 0; i < l->len; i++) nv_list_push_mut(&result, l->data[i]);
    // Insertion sort (good enough for Stage 0)
    NvList *r = result.list;
    for (size_t i = 1; i < r->len; i++) {
        NvVal key = r->data[i]; size_t j = i;
        while (j > 0 && nv_truthy(nv_lt(key, r->data[j-1]))) { r->data[j] = r->data[j-1]; j--; }
        r->data[j] = key;
    }
    return result;
}

static inline NvVal nv_reversed(NvVal lst) {
    NvList *l = lst.list;
    NvVal result = nv_list_new();
    for (size_t i = l->len; i > 0; i--)
        nv_list_push_mut(&result, l->data[i-1]);
    return result;
}

static inline NvVal nv_len_fn(NvVal v) { return nv_len(v); }

static inline NvVal nv_abs_fn(NvVal v) {
    if (v.tag == NV_FLOAT) return nv_float(fabs(v.f));
    return nv_int(v.i < 0 ? -v.i : v.i);
}

static inline NvVal nv_sqrt_fn(NvVal v)  { return nv_float(sqrt(nv_to_f(v)));  }
static inline NvVal nv_floor_fn(NvVal v) { return nv_int((int64_t)floor(nv_to_f(v))); }
static inline NvVal nv_ceil_fn(NvVal v)  { return nv_int((int64_t)ceil(nv_to_f(v)));  }
static inline NvVal nv_round_fn(NvVal v) { return nv_int((int64_t)round(nv_to_f(v))); }

// list.push(item) — returns nil (mutates in place)
static inline NvVal nv_list_push(NvVal lst, NvVal item) {
    nv_list_push_mut(&lst, item); return nv_nil();
}

// list.pop() — removes and returns last element
static inline NvVal nv_list_pop(NvVal lst) {
    if (lst.list->len == 0) return nv_nil();
    return lst.list->data[--lst.list->len];
}

// list.append(item) — alias for push
static inline NvVal nv_list_append(NvVal lst, NvVal item) {
    nv_list_push_mut(&lst, item); return nv_nil();
}

// Polymorphic index set: list[i]=v or map[k]=v
static inline void nv_index_set(NvVal obj, NvVal key, NvVal val) {
    if (obj.tag == NV_LIST) {
        NvList *l = obj.list;
        int64_t idx = (key.tag == NV_INT || key.tag == NV_BOOL) ? key.i : (int64_t)key.f;
        if (idx < 0) idx = (int64_t)l->len + idx;
        if (idx >= 0 && (size_t)idx < l->len) l->data[idx] = val;
    } else if (obj.tag == NV_MAP) {
        NvMap *mp = obj.map;
        for (size_t i = 0; i < mp->len; i++) {
            if (nv_val_eq(mp->data[i].key, key)) { mp->data[i].val = val; return; }
        }
        if (mp->len >= mp->cap) {
            mp->cap = mp->cap ? mp->cap * 2 : 8;
            mp->data = (NvEntry*)NV_REALLOC(mp->data, mp->cap * sizeof(NvEntry));
        }
        mp->data[mp->len++] = (NvEntry){key, val};
    }
}

// list.take(n) — first n elements
static inline NvVal nv_list_take(NvVal lst, NvVal n_val) {
    NvVal r = nv_list_new();
    int64_t n = n_val.i;
    NvList *l = lst.list;
    for (int64_t i = 0; i < n && (size_t)i < l->len; i++)
        nv_list_push_mut(&r, l->data[i]);
    return r;
}

// list.drop(n) — skip first n elements
static inline NvVal nv_list_drop(NvVal lst, NvVal n_val) {
    NvVal r = nv_list_new();
    int64_t n = n_val.i;
    NvList *l = lst.list;
    for (size_t i = (size_t)(n < 0 ? 0 : n); i < l->len; i++)
        nv_list_push_mut(&r, l->data[i]);
    return r;
}

// list + list — concat
static inline NvVal nv_list_concat(NvVal a, NvVal b) {
    NvVal r = nv_list_new();
    NvList *la = a.list, *lb = b.list;
    for (size_t i = 0; i < la->len; i++) nv_list_push_mut(&r, la->data[i]);
    for (size_t i = 0; i < lb->len; i++) nv_list_push_mut(&r, lb->data[i]);
    return r;
}

// list.contains(v)
static inline NvVal nv_list_contains(NvVal lst, NvVal val) {
    NvList *l = lst.list;
    for (size_t i = 0; i < l->len; i++)
        if (nv_val_eq(l->data[i], val)) return nv_bool(1);
    return nv_bool(0);
}
// nv_contains: dispatch on type (str or list)
static inline NvVal nv_contains(NvVal obj, NvVal val) {
    if (obj.tag == NV_STR) return nv_str_contains(obj, val);
    if (obj.tag == NV_LIST) return nv_list_contains(obj, val);
    return nv_bool(0);
}

// map.entries() — list of [key, val] pairs (each a 2-element list)
static inline NvVal nv_map_entries(NvVal m) {
    NvVal r = nv_list_new();
    NvMap *mp = m.map;
    for (size_t i = 0; i < mp->len; i++) {
        NvVal pair = nv_list_of(2, mp->data[i].key, mp->data[i].val);
        nv_list_push_mut(&r, pair);
    }
    return r;
}

// str.chars() — list of single-char strings
static inline NvVal nv_str_chars(NvVal s) {
    NvVal r = nv_list_new();
    const char *p = s.s;
    while (*p) {
        char buf[2] = { *p, '\0' };
        nv_list_push_mut(&r, nv_str(buf));
        p++;
    }
    return r;
}

// str.parse_i64() / str.parse_int()
static inline NvVal nv_str_parse_int(NvVal s) {
    if (s.tag != NV_STR) return nv_int(0);
    return nv_int((int64_t)strtoll(s.s, NULL, 10));
}

// map.set(key, val) — mutate map in place, return the map
static inline NvVal nv_map_set_mut(NvVal m, NvVal key, NvVal val) {
    if (m.tag != NV_MAP) return m;
    NvMap *mp = m.map;
    for (size_t i = 0; i < mp->len; i++) {
        if (nv_val_eq(mp->data[i].key, key)) { mp->data[i].val = val; return m; }
    }
    if (mp->len >= mp->cap) {
        mp->cap = mp->cap ? mp->cap * 2 : 8;
        mp->data = (NvEntry*)NV_REALLOC(mp->data, mp->cap * sizeof(NvEntry));
    }
    mp->data[mp->len++] = (NvEntry){key, val};
    return m;
}

// map merge: copy all entries from src into dst (overwrites existing keys)
static inline NvVal nv_map_merge(NvVal dst, NvVal src) {
    if (dst.tag != NV_MAP || src.tag != NV_MAP) return dst;
    for (size_t i = 0; i < src.map->len; i++) {
        nv_map_set_mut(dst, src.map->data[i].key, src.map->data[i].val);
    }
    return dst;
}

// map copy: return a shallow copy of m
static inline NvVal nv_map_copy(NvVal m) {
    if (m.tag != NV_MAP) return nv_map_new();
    NvVal dst = nv_map_new();
    for (size_t i = 0; i < m.map->len; i++) {
        nv_map_set_mut(dst, m.map->data[i].key, m.map->data[i].val);
    }
    return dst;
}

// identity placeholder function (for bare `_` used as a value)
static inline NvVal __placeholder(NvVal x, NvVal __env) { (void)__env; return x; }

// ─────────────────────────────────────────────────────────────────────────────
// Method dispatch helper macros (used in generated code)
// ─────────────────────────────────────────────────────────────────────────────

// NV_METHOD(obj, "method", arg) — dispatch method call
// Generated code calls these directly as C functions; see codegen.rs for mapping.

// ─────────────────────────────────────────────────────────────────────────────
// M14.4  File I/O stdlib
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_file_write(NvVal path, NvVal contents) {
    if (path.tag != NV_STR || contents.tag != NV_STR) return nv_bool(0);
    FILE *f = fopen(path.s, "wb");
    if (!f) return nv_bool(0);
    fwrite(contents.s, 1, strlen(contents.s), f);
    fclose(f);
    return nv_bool(1);
}

static inline NvVal nv_file_read(NvVal path) {
    if (path.tag != NV_STR) return nv_nil();
    FILE *f = fopen(path.s, "rb");
    if (!f) return nv_nil();
    fseek(f, 0, SEEK_END); long sz = ftell(f); rewind(f);
    char *buf = (char*)NV_MALLOC((size_t)sz + 1);
    fread(buf, 1, (size_t)sz, f); buf[sz] = '\0';
    fclose(f);
    NvVal v = {0}; v.tag = NV_STR; v.s = buf; return v;
}

static inline NvVal nv_file_exists(NvVal path) {
    if (path.tag != NV_STR) return nv_bool(0);
    FILE *f = fopen(path.s, "rb");
    if (!f) return nv_bool(0);
    fclose(f); return nv_bool(1);
}

static inline NvVal nv_file_append(NvVal path, NvVal contents) {
    if (path.tag != NV_STR || contents.tag != NV_STR) return nv_bool(0);
    FILE *f = fopen(path.s, "ab");
    if (!f) return nv_bool(0);
    fwrite(contents.s, 1, strlen(contents.s), f);
    fclose(f);
    return nv_bool(1);
}

// ─────────────────────────────────────────────────────────────────────────────
// M14.2  Channels + spawn  (pthread-backed)
// ─────────────────────────────────────────────────────────────────────────────

#define NV_CHAN_CAP 256   // ring-buffer capacity (power of 2)

struct NvChan {
    NvVal            buf[NV_CHAN_CAP];
    size_t           head, tail, count;
    pthread_mutex_t  mu;
    pthread_cond_t   not_empty;
    pthread_cond_t   not_full;
};

static inline NvVal nv_chan(NvChan *c) {
    NvVal v = {0}; v.tag = NV_CHAN; v.chan = c; return v;
}

static inline NvVal nv_chan_new(void) {
    NvChan *c = (NvChan*)calloc(1, sizeof(NvChan));
    pthread_mutex_init(&c->mu, NULL);
    pthread_cond_init(&c->not_empty, NULL);
    pthread_cond_init(&c->not_full,  NULL);
    return nv_chan(c);
}

static inline NvVal nv_chan_send(NvVal ch, NvVal val) {
    if (ch.tag != NV_CHAN) return nv_nil();
    NvChan *c = ch.chan;
    pthread_mutex_lock(&c->mu);
    while (c->count == NV_CHAN_CAP)
        pthread_cond_wait(&c->not_full, &c->mu);
    c->buf[c->tail] = val;
    c->tail = (c->tail + 1) % NV_CHAN_CAP;
    c->count++;
    pthread_cond_signal(&c->not_empty);
    pthread_mutex_unlock(&c->mu);
    return nv_nil();
}

static inline NvVal nv_chan_recv(NvVal ch) {
    if (ch.tag != NV_CHAN) return nv_nil();
    NvChan *c = ch.chan;
    pthread_mutex_lock(&c->mu);
    while (c->count == 0)
        pthread_cond_wait(&c->not_empty, &c->mu);
    NvVal v = c->buf[c->head];
    c->head = (c->head + 1) % NV_CHAN_CAP;
    c->count--;
    pthread_cond_signal(&c->not_full);
    pthread_mutex_unlock(&c->mu);
    return v;
}

// ── Futures (spawn + await) ────────────────────────────────────────────────────
typedef struct {
    NvVal           result;
    int             done;
    pthread_mutex_t mu;
    pthread_cond_t  cv;
} NvFuture;

typedef struct { NvVal fn; NvVal arg; NvFuture *fut; } _NvSpawnArgs;

static void *_nv_thread_entry(void *raw) {
    _NvSpawnArgs *a = (_NvSpawnArgs*)raw;
    NvVal fn = a->fn, arg = a->arg;
    NvFuture *fut = a->fut;
    NV_FREE(a);
    NvVal result = nv_nil();
    if (fn.tag == NV_FN) {
        NvVal env = fn.clo->env ? *(NvVal*)fn.clo->env : nv_nil();
        result = fn.clo->fn(arg, env);
    }
    pthread_mutex_lock(&fut->mu);
    fut->result = result;
    fut->done   = 1;
    pthread_cond_signal(&fut->cv);
    pthread_mutex_unlock(&fut->mu);
    return NULL;
}

// spawn(fn_val, arg) — creates a pthread; returns a future NvVal (NV_PTR to NvFuture)
static inline NvVal nv_spawn(NvVal fn_val, NvVal arg) {
    NvFuture *fut = (NvFuture*)calloc(1, sizeof(NvFuture));
    pthread_mutex_init(&fut->mu, NULL);
    pthread_cond_init(&fut->cv, NULL);
    _NvSpawnArgs *a = (_NvSpawnArgs*)NV_MALLOC(sizeof(_NvSpawnArgs));
    a->fn  = fn_val;
    a->arg = arg;
    a->fut = fut;
    pthread_t tid;
    pthread_create(&tid, NULL, _nv_thread_entry, a);
    pthread_detach(tid);
    return nv_ptr(fut);  // caller can await_ this
}

// await_(future) — block until the spawned thread finishes, return its result.
// If passed a non-future (e.g. result of a sync async fn), returns it directly.
static inline NvVal nv_await_(NvVal fut_val) {
    if (fut_val.tag != NV_PTR || !fut_val.p) return fut_val;
    NvFuture *fut = (NvFuture*)fut_val.p;
    pthread_mutex_lock(&fut->mu);
    while (!fut->done)
        pthread_cond_wait(&fut->cv, &fut->mu);
    NvVal result = fut->result;
    pthread_mutex_unlock(&fut->mu);
    return result;
}

// ─────────────────────────────────────────────────────────────────────────────
// M14.5  HTTP stdlib  (POSIX sockets, no external deps)
// ─────────────────────────────────────────────────────────────────────────────
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <netdb.h>
#include <unistd.h>
#include <fcntl.h>
#include <errno.h>

// sleep_ms(n) — sleep for n milliseconds
static inline NvVal nv_sleep_ms(NvVal ms) {
    long n = (ms.tag == NV_INT) ? (long)ms.i : (long)ms.f;
    struct timespec ts = { n / 1000, (n % 1000) * 1000000L };
    nanosleep(&ts, NULL);
    return nv_nil();
}

// ── HTTP response builder ─────────────────────────────────────────────────────
// http_response(status, body) → map {status, body}
static inline NvVal nv_http_response(NvVal status, NvVal body) {
    NvVal r = nv_map_new();
    nv_map_set_mut(r, nv_str("status"), status);
    nv_map_set_mut(r, nv_str("body"),   body);
    return r;
}

// ── HTTP client ───────────────────────────────────────────────────────────────
// Parse "http://host:port/path" → fills host_out, port_out, path_out buffers
static void _nv_parse_url(const char *url,
                           char *host_out, int *port_out, char *path_out) {
    *port_out = 80;
    strcpy(path_out, "/");
    const char *p = url;
    if (strncmp(p, "http://", 7) == 0) p += 7;
    // find host end
    const char *slash = strchr(p, '/');
    const char *colon = strchr(p, ':');
    if (colon && (!slash || colon < slash)) {
        size_t hlen = (size_t)(colon - p);
        memcpy(host_out, p, hlen); host_out[hlen] = '\0';
        *port_out = atoi(colon + 1);
    } else {
        size_t hlen = slash ? (size_t)(slash - p) : strlen(p);
        memcpy(host_out, p, hlen); host_out[hlen] = '\0';
    }
    if (slash) strcpy(path_out, slash);
}

static NvVal _nv_http_request(const char *method, const char *url,
                               const char *req_body) {
    char host[256], path[1024];
    int  port;
    _nv_parse_url(url, host, &port, path);

    struct addrinfo hints = {0}, *res = NULL;
    hints.ai_family   = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    char port_str[16]; snprintf(port_str, sizeof(port_str), "%d", port);
    if (getaddrinfo(host, port_str, &hints, &res) != 0)
        return nv_str("(connection error)");

    int fd = socket(res->ai_family, res->ai_socktype, res->ai_protocol);
    if (fd < 0) { freeaddrinfo(res); return nv_str("(socket error)"); }
    if (connect(fd, res->ai_addr, res->ai_addrlen) < 0) {
        close(fd); freeaddrinfo(res); return nv_str("(connect error)");
    }
    freeaddrinfo(res);

    // Build request
    size_t body_len = req_body ? strlen(req_body) : 0;
    char req[4096];
    int  rlen = snprintf(req, sizeof(req),
        "%s %s HTTP/1.0\r\n"
        "Host: %s\r\n"
        "Content-Type: application/json\r\n"
        "Content-Length: %zu\r\n"
        "Connection: close\r\n"
        "\r\n", method, path, host, body_len);
    send(fd, req, (size_t)rlen, 0);
    if (body_len) send(fd, req_body, body_len, 0);

    // Read response
    char  buf[65536]; size_t total = 0;
    ssize_t n;
    while (total < sizeof(buf) - 1 && (n = recv(fd, buf + total, sizeof(buf) - total - 1, 0)) > 0)
        total += (size_t)n;
    buf[total] = '\0';
    close(fd);

    // Skip HTTP headers (find \r\n\r\n)
    char *body = strstr(buf, "\r\n\r\n");
    if (body) body += 4; else body = buf;
    return nv_str(body);
}

static inline NvVal nv_http_get(NvVal url) {
    if (url.tag != NV_STR) return nv_nil();
    return _nv_http_request("GET", url.s, NULL);
}

static inline NvVal nv_http_post(NvVal url, NvVal body) {
    if (url.tag != NV_STR) return nv_nil();
    const char *b = (body.tag == NV_STR) ? body.s : "";
    return _nv_http_request("POST", url.s, b);
}

// ── HTTP server ───────────────────────────────────────────────────────────────
// Serve one accepted connection: parse request, call handler fn, send response.
static void _nv_handle_conn(int client_fd, NvVal handler) {
    char buf[65536]; size_t total = 0;
    ssize_t n;
    // Read until we have the full headers
    while (total < sizeof(buf) - 1) {
        n = recv(client_fd, buf + total, sizeof(buf) - total - 1, 0);
        if (n <= 0) break;
        total += (size_t)n;
        if (strstr(buf, "\r\n\r\n")) break;
    }
    buf[total] = '\0';

    // Parse first line: METHOD /path HTTP/1.x
    char method[16] = "GET", path[512] = "/", http_ver[16] = "HTTP/1.0";
    sscanf(buf, "%15s %511s %15s", method, path, http_ver);

    // Find header/body split
    char *hdr_end = strstr(buf, "\r\n\r\n");
    char *body_start = hdr_end ? hdr_end + 4 : buf + total;

    // Parse Content-Length so we can read the full body
    long content_length = 0;
    char *cl = strcasestr(buf, "Content-Length:");
    if (cl && cl < (hdr_end ? hdr_end : buf + total))
        content_length = atol(cl + 15);

    // Read remaining body bytes if needed
    size_t header_len  = (size_t)(body_start - buf);
    size_t body_so_far = total - header_len;
    while ((long)body_so_far < content_length && total < sizeof(buf) - 1) {
        n = recv(client_fd, buf + total, sizeof(buf) - total - 1, 0);
        if (n <= 0) break;
        total      += (size_t)n;
        body_so_far += (size_t)n;
    }
    buf[total] = '\0';
    // Null-terminate the body at content_length boundary
    if (content_length > 0 && header_len + (size_t)content_length < sizeof(buf))
        buf[header_len + (size_t)content_length] = '\0';

    const char *req_body = body_start;

    // Build request map for handler
    NvVal req = nv_map_new();
    nv_map_set_mut(req, nv_str("method"), nv_str(method));
    nv_map_set_mut(req, nv_str("path"),   nv_str(path));
    nv_map_set_mut(req, nv_str("body"),   nv_str(req_body));

    // Call handler
    NvVal resp = nv_nil();
    if (handler.tag == NV_FN) {
        NvVal env = handler.clo->env ? *(NvVal*)handler.clo->env : nv_nil();
        resp = handler.clo->fn(req, env);
    }

    // Extract status + body from response
    int status = 200;
    const char *resp_body = "";
    if (resp.tag == NV_MAP) {
        NvVal sv = nv_map_get_opt(resp, nv_str("status"));
        if (sv.tag == NV_INT) status = (int)sv.i;
        NvVal bv = nv_map_get_opt(resp, nv_str("body"));
        if (bv.tag == NV_STR) resp_body = bv.s;
    } else if (resp.tag == NV_STR) {
        resp_body = resp.s;
    }

    const char *status_str = (status == 200) ? "OK"
                           : (status == 404) ? "Not Found"
                           : (status == 500) ? "Internal Server Error"
                           : "OK";
    size_t blen = strlen(resp_body);
    char hdr[512];
    int  hlen = snprintf(hdr, sizeof(hdr),
        "HTTP/1.0 %d %s\r\n"
        "Content-Type: application/json\r\n"
        "Content-Length: %zu\r\n"
        "Connection: close\r\n"
        "Access-Control-Allow-Origin: *\r\n"
        "Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n"
        "Access-Control-Allow-Headers: Content-Type\r\n"
        "\r\n", status, status_str, blen);
    send(client_fd, hdr, (size_t)hlen, 0);
    if (blen) send(client_fd, resp_body, blen, 0);
    close(client_fd);
}

// ── Per-connection thread args ───────────────────────────────────────────────
typedef struct { int fd; NvVal handler; } _NvConnArgs;

static void *_nv_conn_thread(void *raw) {
    _NvConnArgs *a = (_NvConnArgs*)raw;
    int fd = a->fd; NvVal h = a->handler; NV_FREE(a);
    nv_arena_begin();
    _nv_handle_conn(fd, h);
    nv_arena_end();
    return NULL;
}

// http_serve(port, handler) — multi-threaded server; call from a spawned thread
static inline NvVal nv_http_serve(NvVal port_val, NvVal handler) {
    int port = (port_val.tag == NV_INT) ? (int)port_val.i : 8080;
    int server_fd = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    int opt = 1;
    setsockopt(server_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    struct sockaddr_in addr = {0};
    addr.sin_family      = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port        = htons((uint16_t)port);
    if (bind(server_fd, (struct sockaddr*)&addr, sizeof(addr)) < 0) {
        close(server_fd); return nv_nil();
    }
    listen(server_fd, 64);

    while (1) {
        struct sockaddr_in client_addr; socklen_t clen = sizeof(client_addr);
        int client_fd = accept(server_fd, (struct sockaddr*)&client_addr, &clen);
        if (client_fd < 0) continue;
        _NvConnArgs *ca = (_NvConnArgs*)NV_MALLOC(sizeof(_NvConnArgs));
        ca->fd = client_fd; ca->handler = handler;
        pthread_t tid;
        pthread_create(&tid, NULL, _nv_conn_thread, ca);
        pthread_detach(tid);
    }
    close(server_fd);
    return nv_nil();
}

// ─────────────────────────────────────────────────────────────────────────────
// Math builtins (M14+)
// ─────────────────────────────────────────────────────────────────────────────
#include <time.h>

static inline NvVal nv_sin_fn(NvVal v)   { return nv_float(sin(nv_to_f(v)));   }
static inline NvVal nv_cos_fn(NvVal v)   { return nv_float(cos(nv_to_f(v)));   }
static inline NvVal nv_tan_fn(NvVal v)   { return nv_float(tan(nv_to_f(v)));   }
static inline NvVal nv_exp_fn(NvVal v)   { return nv_float(exp(nv_to_f(v)));   }
static inline NvVal nv_log2_fn(NvVal v)  { return nv_float(log2(nv_to_f(v)));  }
static inline NvVal nv_log10_fn(NvVal v) { return nv_float(log10(nv_to_f(v))); }
// nv_log_fn: forward-declare nv_tensor_log so it can handle tensor or scalar
static NvVal nv_tensor_log(NvVal t);
static inline NvVal nv_log_fn(NvVal v) {
    if (v.tag == NV_MAP) return nv_tensor_log(v);  // tensor element-wise log
    return nv_float(log(nv_to_f(v)));
}
static inline NvVal nv_pow_fn(NvVal a, NvVal b) { return nv_float(pow(nv_to_f(a), nv_to_f(b))); }
static inline NvVal nv_hypot_fn(NvVal a, NvVal b) { return nv_float(hypot(nv_to_f(a), nv_to_f(b))); }
static inline NvVal nv_atan2_fn(NvVal a, NvVal b) { return nv_float(atan2(nv_to_f(a), nv_to_f(b))); }

// ─────────────────────────────────────────────────────────────────────────────
// System builtins (M14+)
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_time_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return nv_int((int64_t)(ts.tv_sec * 1000LL + ts.tv_nsec / 1000000LL));
}
static inline NvVal nv_exit_fn(NvVal code) {
    exit((int)(code.tag == NV_INT ? code.i : 0));
    return nv_nil();
}
static inline NvVal nv_env_get(NvVal name) {
    if (name.tag != NV_STR) return nv_nil();
    const char *v = getenv(name.s);
    return v ? nv_str(v) : nv_nil();
}
static inline NvVal nv_cli_args(void) {
    extern int _nv_argc; extern char **_nv_argv;
    NvVal lst = nv_list_new();
    for (int i = 0; i < _nv_argc; i++)
        nv_list_push_mut(&lst, nv_str(_nv_argv[i]));
    return lst;
}

// ─────────────────────────────────────────────────────────────────────────────
// OS / filesystem builtins
// ─────────────────────────────────────────────────────────────────────────────
#include <sys/stat.h>
#include <dirent.h>
#include <unistd.h>
#include <errno.h>

// os_getcwd() → string
static inline NvVal nv_os_getcwd(void) {
    char buf[4096];
    if (getcwd(buf, sizeof(buf))) return nv_str(buf);
    return nv_nil();
}

// os_exists(path) → bool
static inline NvVal nv_os_exists(NvVal path) {
    if (path.tag != NV_STR) return nv_bool(0);
    struct stat st;
    return nv_bool(stat(path.s, &st) == 0);
}

// os_is_file(path) → bool
static inline NvVal nv_os_is_file(NvVal path) {
    if (path.tag != NV_STR) return nv_bool(0);
    struct stat st;
    if (stat(path.s, &st) != 0) return nv_bool(0);
    return nv_bool(S_ISREG(st.st_mode));
}

// os_is_dir(path) → bool
static inline NvVal nv_os_is_dir(NvVal path) {
    if (path.tag != NV_STR) return nv_bool(0);
    struct stat st;
    if (stat(path.s, &st) != 0) return nv_bool(0);
    return nv_bool(S_ISDIR(st.st_mode));
}

// os_listdir(path) → list of strings (filenames, not full paths)
static inline NvVal nv_os_listdir(NvVal path) {
    const char *p = (path.tag == NV_STR) ? path.s : ".";
    DIR *d = opendir(p);
    NvVal lst = nv_list_new();
    if (!d) return lst;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        nv_list_push_mut(&lst, nv_str(e->d_name));
    }
    closedir(d);
    return lst;
}

// os_mkdir(path) → bool (true on success)
static inline NvVal nv_os_mkdir(NvVal path) {
    if (path.tag != NV_STR) return nv_bool(0);
    int r = mkdir(path.s, 0755);
    return nv_bool(r == 0 || errno == EEXIST);
}

// os_remove(path) → bool
static inline NvVal nv_os_remove(NvVal path) {
    if (path.tag != NV_STR) return nv_bool(0);
    return nv_bool(remove(path.s) == 0);
}

// os_rename(src, dst) → bool
static inline NvVal nv_os_rename(NvVal src, NvVal dst) {
    if (src.tag != NV_STR || dst.tag != NV_STR) return nv_bool(0);
    return nv_bool(rename(src.s, dst.s) == 0);
}

// os_getenv(name) → string or nil
static inline NvVal nv_os_getenv(NvVal name) {
    return nv_env_get(name);
}

// os_setenv(name, val) → bool
static inline NvVal nv_os_setenv(NvVal name, NvVal val) {
    if (name.tag != NV_STR || val.tag != NV_STR) return nv_bool(0);
    return nv_bool(setenv(name.s, val.s, 1) == 0);
}

// os_system(cmd) → int exit code
static inline NvVal nv_os_system(NvVal cmd) {
    if (cmd.tag != NV_STR) return nv_int(-1);
    return nv_int(system(cmd.s));
}

// os_file_size(path) → int (bytes) or -1
static inline NvVal nv_os_file_size(NvVal path) {
    if (path.tag != NV_STR) return nv_int(-1);
    struct stat st;
    if (stat(path.s, &st) != 0) return nv_int(-1);
    return nv_int((int64_t)st.st_size);
}

// ─────────────────────────────────────────────────────────────────────────────
// Time builtins
// ─────────────────────────────────────────────────────────────────────────────

// time_now_ms() → milliseconds since epoch
static inline NvVal nv_time_now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return nv_int((int64_t)(ts.tv_sec * 1000LL + ts.tv_nsec / 1000000LL));
}

// time_now_us() → microseconds since epoch
static inline NvVal nv_time_now_us(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return nv_int((int64_t)(ts.tv_sec * 1000000LL + ts.tv_nsec / 1000LL));
}

// time_now_sec() → seconds since epoch (float)
static inline NvVal nv_time_now_sec(void) {
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);
    return nv_float((double)ts.tv_sec + (double)ts.tv_nsec / 1e9);
}

// sleep_sec(s) — sleep for s seconds (float OK)
static inline NvVal nv_sleep_sec(NvVal s) {
    double secs = nv_to_f(s);
    if (secs <= 0) return nv_nil();
    struct timespec ts;
    ts.tv_sec  = (time_t)secs;
    ts.tv_nsec = (long)((secs - (double)ts.tv_sec) * 1e9);
    nanosleep(&ts, NULL);
    return nv_nil();
}

// nv_clock_ns() → nanoseconds (CLOCK_MONOTONIC) — high-res benchmarking
// NvFn-compatible: takes (arg, env), ignores both, returns nv_int(ns)
static inline NvVal nv_clock_ns(NvVal _a, NvVal _e) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return nv_int((int64_t)(ts.tv_sec * 1000000000LL + ts.tv_nsec));
}

// nv_shell(cmd, env) → exit code as NvVal int
// NvFn-compatible: first arg is the command string
static inline NvVal nv_shell(NvVal cmd, NvVal _e) {
    const char *s = (cmd.tag == NV_STR) ? cmd.s : "";
    if (!s || !*s) return nv_int(1);
    int rc = system(s);
    return nv_int((int64_t)WEXITSTATUS(rc));
}

// time_format(ms_since_epoch) → "YYYY-MM-DD HH:MM:SS"
static inline NvVal nv_time_format(NvVal ms) {
    time_t t = (time_t)(ms.tag == NV_INT ? ms.i / 1000 : (int64_t)nv_to_f(ms) / 1000);
    struct tm *tm_info = localtime(&t);
    char buf[32];
    strftime(buf, sizeof(buf), "%Y-%m-%d %H:%M:%S", tm_info);
    return nv_str(buf);
}

// time_format_iso(ms_since_epoch) → "YYYY-MM-DDTHH:MM:SSZ"
static inline NvVal nv_time_format_iso(NvVal ms) {
    time_t t = (time_t)(ms.tag == NV_INT ? ms.i / 1000 : (int64_t)nv_to_f(ms) / 1000);
    struct tm *tm_info = gmtime(&t);
    char buf[32];
    strftime(buf, sizeof(buf), "%Y-%m-%dT%H:%M:%SZ", tm_info);
    return nv_str(buf);
}

// ─────────────────────────────────────────────────────────────────────────────
// String builtins (M14+): repeat, index_of, slice, format
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_str_repeat(NvVal s, NvVal n_val) {
    if (s.tag != NV_STR) return nv_str("");
    size_t n = (size_t)(n_val.tag == NV_INT && n_val.i > 0 ? n_val.i : 0);
    size_t sl = strlen(s.s);
    char *out = (char*)NV_MALLOC(sl * n + 1); out[0] = '\0';
    for (size_t i = 0; i < n; i++) strcat(out, s.s);
    NvVal r = nv_str(out); NV_FREE(out); return r;
}
static inline NvVal nv_str_index_of(NvVal s, NvVal sub) {
    if (s.tag != NV_STR || sub.tag != NV_STR) return nv_int(-1);
    char *p = strstr(s.s, sub.s);
    return p ? nv_int((int64_t)(p - s.s)) : nv_int(-1);
}
static inline NvVal nv_str_slice(NvVal s, NvVal start_v, NvVal end_v) {
    if (s.tag != NV_STR) return nv_str("");
    size_t slen = strlen(s.s);
    int64_t st = start_v.tag == NV_INT ? start_v.i : 0;
    int64_t en = end_v.tag == NV_INT ? end_v.i : (int64_t)slen;
    if (st < 0) st = 0;
    if (en > (int64_t)slen) en = (int64_t)slen;
    if (st >= en) return nv_str("");
    size_t sz = (size_t)(en - st);
    char *buf = (char*)NV_MALLOC(sz + 1);
    memcpy(buf, s.s + st, sz); buf[sz] = '\0';
    NvVal r = nv_str(buf); NV_FREE(buf); return r;
}

// ─────────────────────────────────────────────────────────────────────────────
// List builtins (M14+): first, last, zip, flatten, unique, enumerate
// ─────────────────────────────────────────────────────────────────────────────

static inline NvVal nv_list_first(NvVal lst) {
    if (lst.tag != NV_LIST || lst.list->len == 0) return nv_nil();
    return lst.list->data[0];
}
static inline NvVal nv_list_last(NvVal lst) {
    if (lst.tag != NV_LIST || lst.list->len == 0) return nv_nil();
    return lst.list->data[lst.list->len - 1];
}
static inline NvVal nv_list_zip(NvVal a, NvVal b) {
    if (a.tag != NV_LIST || b.tag != NV_LIST) return nv_list_new();
    size_t n = a.list->len < b.list->len ? a.list->len : b.list->len;
    NvVal result = nv_list_new();
    for (size_t i = 0; i < n; i++) {
        NvVal pair = nv_list_new();
        nv_list_push_mut(&pair, a.list->data[i]);
        nv_list_push_mut(&pair, b.list->data[i]);
        nv_list_push_mut(&result, pair);
    }
    return result;
}
static inline NvVal nv_list_flatten(NvVal lst) {
    if (lst.tag != NV_LIST) return nv_list_new();
    NvVal result = nv_list_new();
    for (size_t i = 0; i < lst.list->len; i++) {
        NvVal item = lst.list->data[i];
        if (item.tag == NV_LIST)
            for (size_t j = 0; j < item.list->len; j++)
                nv_list_push_mut(&result, item.list->data[j]);
        else
            nv_list_push_mut(&result, item);
    }
    return result;
}
static inline NvVal nv_list_unique(NvVal lst) {
    if (lst.tag != NV_LIST) return nv_list_new();
    NvVal result = nv_list_new();
    for (size_t i = 0; i < lst.list->len; i++) {
        NvVal item = lst.list->data[i];
        int found = 0;
        for (size_t j = 0; j < result.list->len; j++) {
            NvVal r = result.list->data[j];
            if (r.tag == item.tag) {
                if (item.tag == NV_INT   && r.i == item.i) { found = 1; break; }
                if (item.tag == NV_FLOAT && r.f == item.f) { found = 1; break; }
                if (item.tag == NV_STR   && strcmp(r.s, item.s) == 0) { found = 1; break; }
                if (item.tag == NV_BOOL  && r.i == item.i) { found = 1; break; }
                if (item.tag == NV_NIL)  { found = 1; break; }
            }
        }
        if (!found) nv_list_push_mut(&result, item);
    }
    return result;
}
static inline NvVal nv_list_enumerate(NvVal lst) {
    if (lst.tag != NV_LIST) return nv_list_new();
    NvVal result = nv_list_new();
    for (size_t i = 0; i < lst.list->len; i++) {
        NvVal pair = nv_list_new();
        nv_list_push_mut(&pair, nv_int((int64_t)i));
        nv_list_push_mut(&pair, lst.list->data[i]);
        nv_list_push_mut(&result, pair);
    }
    return result;
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON  (M14+) — minimal recursive descent parser + stringifier
// ─────────────────────────────────────────────────────────────────────────────

static NvVal _nv_json_parse_val(const char **p);
static inline void _nv_skip_ws(const char **p) { while (**p && isspace((unsigned char)**p)) (*p)++; }
static inline void nv_throw(NvVal val); // forward declaration for use in json parser

static inline NvVal _nv_json_parse_str(const char **p) {
    (*p)++; // skip opening "
    size_t cap = 64; size_t len = 0;
    char *buf = (char*)NV_MALLOC(cap);
    while (**p && **p != '"') {
        if (**p == '\\') {
            (*p)++;
            char c = **p;
            if (c == 'n') buf[len++] = '\n';
            else if (c == 't') buf[len++] = '\t';
            else if (c == 'r') buf[len++] = '\r';
            else buf[len++] = c;
        } else {
            buf[len++] = **p;
        }
        (*p)++;
        if (len + 2 >= cap) { cap *= 2; buf = (char*)NV_REALLOC(buf, cap); }
    }
    if (**p == '"') (*p)++;
    buf[len] = '\0';
    NvVal r = nv_str(buf); NV_FREE(buf); return r;
}

static inline NvVal _nv_json_parse_val(const char **p) {
    _nv_skip_ws(p);
    if (**p == '"') return _nv_json_parse_str(p);
    if (**p == '[') {
        (*p)++;
        NvVal lst = nv_list_new();
        _nv_skip_ws(p);
        if (**p == ']') { (*p)++; return lst; }
        while (1) {
            nv_list_push_mut(&lst, _nv_json_parse_val(p));
            _nv_skip_ws(p);
            if (**p == ',') { (*p)++; continue; }
            if (**p == ']') { (*p)++; break; }
            break;
        }
        return lst;
    }
    if (**p == '{') {
        (*p)++;
        NvVal map = nv_map_new();
        _nv_skip_ws(p);
        if (**p == '}') { (*p)++; return map; }
        while (1) {
            _nv_skip_ws(p);
            NvVal key = _nv_json_parse_str(p);
            _nv_skip_ws(p);
            if (**p == ':') (*p)++;
            NvVal val = _nv_json_parse_val(p);
            nv_map_set_mut(map, key, val);
            _nv_skip_ws(p);
            if (**p == ',') { (*p)++; continue; }
            if (**p == '}') { (*p)++; break; }
            break;
        }
        return map;
    }
    if (strncmp(*p, "true", 4) == 0)  { (*p) += 4; return nv_bool(1); }
    if (strncmp(*p, "false", 5) == 0) { (*p) += 5; return nv_bool(0); }
    if (strncmp(*p, "null", 4) == 0)  { (*p) += 4; return nv_nil(); }
    // number
    char *end;
    int64_t i = (int64_t)strtoll(*p, &end, 10);
    if (end != *p) {
        // Check if it's actually a float (has '.' or 'e' before next non-numeric)
        if (*end == '.' || *end == 'e' || *end == 'E') {
            double d = strtod(*p, &end); *p = end; return nv_float(d);
        }
        *p = end; return nv_int(i);
    }
    double d = strtod(*p, &end);
    if (end != *p) { *p = end; return nv_float(d); }
    return nv_nil(); // unrecognized token
}

static inline NvVal nv_json_parse(NvVal s) {
    if (s.tag != NV_STR) { nv_throw(nv_str("json_parse: expected string")); return nv_nil(); }
    const char *orig = s.s;
    const char *p = orig;
    // skip leading whitespace
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r') p++;
    if (*p == '\0') { nv_throw(nv_str("json_parse: empty input")); return nv_nil(); }
    const char *start = p;
    NvVal result = _nv_json_parse_val(&p);
    // If parser didn't advance at all, or returned nil for non-null input, throw
    if (p == start || (result.tag == NV_NIL && strncmp(start, "null", 4) != 0)) {
        nv_throw(nv_str("json_parse: invalid JSON"));
        return nv_nil();
    }
    return result;
}

// Forward declare nv_json_stringify for recursion
static NvVal nv_json_stringify(NvVal v);

static inline NvVal nv_json_stringify(NvVal v) {
    char buf[128];
    switch (v.tag) {
        case NV_NIL:   return nv_str("null");
        case NV_BOOL:  return nv_str(v.i ? "true" : "false");
        case NV_INT:   snprintf(buf, sizeof(buf), "%lld", (long long)v.i); return nv_str(buf);
        case NV_FLOAT: snprintf(buf, sizeof(buf), "%g", v.f); return nv_str(buf);
        case NV_STR: {
            size_t sl = strlen(v.s);
            char *out = (char*)NV_MALLOC(sl * 2 + 4);
            size_t oi = 0; out[oi++] = '"';
            for (size_t i = 0; i < sl; i++) {
                char c = v.s[i];
                if (c == '"')       { out[oi++] = '\\'; out[oi++] = '"'; }
                else if (c == '\\') { out[oi++] = '\\'; out[oi++] = '\\'; }
                else if (c == '\n') { out[oi++] = '\\'; out[oi++] = 'n'; }
                else if (c == '\r') { out[oi++] = '\\'; out[oi++] = 'r'; }
                else if (c == '\t') { out[oi++] = '\\'; out[oi++] = 't'; }
                else out[oi++] = c;
            }
            out[oi++] = '"'; out[oi] = '\0';
            NvVal r = nv_str(out); NV_FREE(out); return r;
        }
        case NV_LIST: {
            size_t n = v.list->len;
            if (n == 0) return nv_str("[]");
            // build string
            size_t cap = 256; size_t len = 0;
            char *out = (char*)NV_MALLOC(cap); out[len++] = '[';
            for (size_t i = 0; i < n; i++) {
                NvVal elem = nv_json_stringify(v.list->data[i]);
                size_t el = strlen(elem.s);
                while (len + el + 4 >= cap) { cap *= 2; out = (char*)NV_REALLOC(out, cap); }
                memcpy(out + len, elem.s, el); len += el;
                if (i + 1 < n) out[len++] = ',';
            }
            out[len++] = ']'; out[len] = '\0';
            NvVal r = nv_str(out); NV_FREE(out); return r;
        }
        case NV_MAP: {
            size_t n = v.map->len;
            if (n == 0) return nv_str("{}");
            size_t cap = 256; size_t len = 0;
            char *out = (char*)NV_MALLOC(cap); out[len++] = '{';
            for (size_t i = 0; i < n; i++) {
                NvVal kv = nv_json_stringify(v.map->data[i].key);
                NvVal vv = nv_json_stringify(v.map->data[i].val);
                size_t kl = strlen(kv.s), vl = strlen(vv.s);
                while (len + kl + vl + 8 >= cap) { cap *= 2; out = (char*)NV_REALLOC(out, cap); }
                memcpy(out + len, kv.s, kl); len += kl;
                out[len++] = ':';
                memcpy(out + len, vv.s, vl); len += vl;
                if (i + 1 < n) out[len++] = ',';
            }
            out[len++] = '}'; out[len] = '\0';
            NvVal r = nv_str(out); NV_FREE(out); return r;
        }
        default: return nv_str("null");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// try/catch/throw  (M14+ — setjmp-based)
// ─────────────────────────────────────────────────────────────────────────────
#include <setjmp.h>

typedef struct _NvTryFrame {
    jmp_buf jbuf;
    struct _NvTryFrame *prev;
    NvVal thrown;
} _NvTryFrame;

static _NvTryFrame *_nv_try_top = NULL;

static inline void nv_throw(NvVal val) {
    if (_nv_try_top) {
        _nv_try_top->thrown = val;
        longjmp(_nv_try_top->jbuf, 1);
    }
    // Uncaught: print and abort
    NvVal s = nv_to_str(val);
    fprintf(stderr, "Uncaught throw: %s\n", s.s);
    _NV_FATAL_EXIT();
}

// Cleanup function: restores _nv_try_top when the frame goes out of scope.
// This fires on any exit path — return, goto, or normal fall-through —
// so early returns inside NV_TRY_BEGIN...NV_TRY_END are safe.
static inline void _nv_try_cleanup(void *p) {
    _NvTryFrame *f = (_NvTryFrame *)p;
    // Only restore if this frame is still on top (it might have already been
    // popped by a NV_TRY_CATCH or NV_TRY_END path).
    if (_nv_try_top == f) _nv_try_top = f->prev;
}

// Macro helpers for try/catch emission
#define NV_TRY_BEGIN \
    do { __attribute__((cleanup(_nv_try_cleanup))) _NvTryFrame _frame; \
    _frame.prev = _nv_try_top; _frame.thrown = nv_nil(); _nv_try_top = &_frame; \
    if (setjmp(_frame.jbuf) == 0) {

#define NV_TRY_CATCH(var) \
    } else { NvVal var = _frame.thrown; _nv_try_top = _frame.prev;

#define NV_TRY_END \
    } _nv_try_top = _frame.prev; } while(0)

// ─────────────────────────────────────────────────────────────────────────────
// POSIX Regex  (requires -lm already linked; regex.h is POSIX)
// ─────────────────────────────────────────────────────────────────────────────
#include <regex.h>

// regex_match(pattern, str) → bool
static inline NvVal nv_regex_match(NvVal pat, NvVal str) {
    if (pat.tag != NV_STR || str.tag != NV_STR) return nv_bool(0);
    regex_t re; int r = regcomp(&re, pat.s, REG_EXTENDED | REG_NOSUB);
    if (r) return nv_bool(0);
    int ok = regexec(&re, str.s, 0, NULL, 0) == 0;
    regfree(&re); return nv_bool(ok);
}

// regex_find(pattern, str) → first match string or nil
static inline NvVal nv_regex_find(NvVal pat, NvVal str) {
    if (pat.tag != NV_STR || str.tag != NV_STR) return nv_nil();
    regex_t re; int r = regcomp(&re, pat.s, REG_EXTENDED);
    if (r) return nv_nil();
    regmatch_t m[1];
    if (regexec(&re, str.s, 1, m, 0) != 0) { regfree(&re); return nv_nil(); }
    int len = (int)(m[0].rm_eo - m[0].rm_so);
    char *buf = (char*)malloc(len + 1);
    memcpy(buf, str.s + m[0].rm_so, len); buf[len] = '\0';
    regfree(&re);
    NvVal v = nv_str(buf); free(buf); return v;
}

// regex_find_all(pattern, str) → list of all match strings
static inline NvVal nv_regex_find_all(NvVal pat, NvVal str) {
    NvVal lst = nv_list_new(); if (pat.tag != NV_STR || str.tag != NV_STR) return lst;
    regex_t re; if (regcomp(&re, pat.s, REG_EXTENDED) != 0) return lst;
    const char *p = str.s;
    regmatch_t m[1];
    while (*p && regexec(&re, p, 1, m, 0) == 0) {
        int len = (int)(m[0].rm_eo - m[0].rm_so);
        if (len == 0) { p++; continue; }
        char *buf = (char*)malloc(len + 1);
        memcpy(buf, p + m[0].rm_so, len); buf[len] = '\0';
        nv_list_push(lst, nv_str(buf)); free(buf);
        p += m[0].rm_eo;
    }
    regfree(&re); return lst;
}

// regex_replace(pattern, str, repl) → string with all matches replaced by repl
static inline NvVal nv_regex_replace(NvVal pat, NvVal str, NvVal repl) {
    if (pat.tag != NV_STR || str.tag != NV_STR || repl.tag != NV_STR) return str;
    regex_t re; if (regcomp(&re, pat.s, REG_EXTENDED) != 0) return str;
    const char *p = str.s; const char *rp = repl.s; size_t rlen = strlen(rp);
    char *out = (char*)malloc(1); out[0] = '\0'; size_t outlen = 0;
    regmatch_t m[1];
    while (*p && regexec(&re, p, 1, m, 0) == 0) {
        int plen = (int)m[0].rm_so; int mlen = (int)(m[0].rm_eo - m[0].rm_so);
        out = (char*)realloc(out, outlen + plen + rlen + 1);
        memcpy(out + outlen, p, plen); outlen += plen;
        memcpy(out + outlen, rp, rlen); outlen += rlen;
        out[outlen] = '\0';
        p += m[0].rm_eo;
        if (mlen == 0) { if (*p) { out = (char*)realloc(out, outlen+2); out[outlen++] = *p++; out[outlen]='\0'; } else break; }
    }
    out = (char*)realloc(out, outlen + strlen(p) + 1);
    strcpy(out + outlen, p);
    regfree(&re);
    NvVal v = nv_str(out); free(out); return v;
}

// regex_split(pattern, str) → list of substrings split on pattern
static inline NvVal nv_regex_split(NvVal pat, NvVal str) {
    NvVal lst = nv_list_new(); if (pat.tag != NV_STR || str.tag != NV_STR) return lst;
    regex_t re; if (regcomp(&re, pat.s, REG_EXTENDED) != 0) return lst;
    const char *p = str.s; regmatch_t m[1];
    while (regexec(&re, p, 1, m, 0) == 0) {
        int plen = (int)m[0].rm_so; int mlen = (int)(m[0].rm_eo - m[0].rm_so);
        char *buf = (char*)malloc(plen + 1);
        memcpy(buf, p, plen); buf[plen] = '\0';
        nv_list_push(lst, nv_str(buf)); free(buf);
        p += m[0].rm_eo;
        if (mlen == 0 && *p) { p++; }
        else if (mlen == 0) break;
    }
    nv_list_push(lst, nv_str((char*)p));
    regfree(&re); return lst;
}

// ─────────────────────────────────────────────────────────────────────────────
// SHA-256  (self-contained, no OpenSSL needed)
// ─────────────────────────────────────────────────────────────────────────────
static inline void _nv_sha256(const uint8_t *data, size_t len, uint8_t out[32]) {
    static const uint32_t K[64] = {
        0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,
        0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,
        0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,
        0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,
        0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,
        0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,
        0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,
        0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2
    };
    uint32_t h[8] = {0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,
                     0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19};
    #define RR(x,n) (((x)>>(n))|((x)<<(32-(n))))
    #define CH(x,y,z) (((x)&(y))^(~(x)&(z)))
    #define MAJ(x,y,z) (((x)&(y))^((x)&(z))^((y)&(z)))
    #define S0(x) (RR(x,2)^RR(x,13)^RR(x,22))
    #define S1(x) (RR(x,6)^RR(x,11)^RR(x,25))
    #define G0(x) (RR(x,7)^RR(x,18)^((x)>>3))
    #define G1(x) (RR(x,17)^RR(x,19)^((x)>>10))
    uint8_t *msg = (uint8_t*)malloc(len + 128);
    memcpy(msg, data, len);
    size_t mlen = len; msg[mlen++] = 0x80;
    while ((mlen & 63) != 56) msg[mlen++] = 0;
    uint64_t bits = (uint64_t)len * 8;
    for (int i = 7; i >= 0; i--) { msg[mlen++] = (uint8_t)(bits >> (i*8)); }
    for (size_t blk = 0; blk < mlen; blk += 64) {
        uint32_t w[64];
        for (int i = 0; i < 16; i++)
            w[i] = ((uint32_t)msg[blk+i*4]<<24)|((uint32_t)msg[blk+i*4+1]<<16)|
                   ((uint32_t)msg[blk+i*4+2]<<8)|(uint32_t)msg[blk+i*4+3];
        for (int i = 16; i < 64; i++) w[i] = G1(w[i-2])+w[i-7]+G0(w[i-15])+w[i-16];
        uint32_t a=h[0],b=h[1],c=h[2],d=h[3],e=h[4],f=h[5],g=h[6],hh=h[7];
        for (int i = 0; i < 64; i++) {
            uint32_t t1 = hh+S1(e)+CH(e,f,g)+K[i]+w[i];
            uint32_t t2 = S0(a)+MAJ(a,b,c);
            hh=g; g=f; f=e; e=d+t1; d=c; c=b; b=a; a=t1+t2;
        }
        h[0]+=a; h[1]+=b; h[2]+=c; h[3]+=d; h[4]+=e; h[5]+=f; h[6]+=g; h[7]+=hh;
    }
    free(msg);
    for (int i = 0; i < 8; i++) {
        out[i*4+0]=(uint8_t)(h[i]>>24); out[i*4+1]=(uint8_t)(h[i]>>16);
        out[i*4+2]=(uint8_t)(h[i]>>8);  out[i*4+3]=(uint8_t)(h[i]);
    }
    #undef RR
    #undef CH
    #undef MAJ
    #undef S0
    #undef S1
    #undef G0
    #undef G1
}

// sha256(str) → hex string
static inline NvVal nv_sha256(NvVal s) {
    if (s.tag != NV_STR) return nv_str("(not a string)");
    uint8_t digest[32]; _nv_sha256((const uint8_t*)s.s, strlen(s.s), digest);
    char hex[65]; static const char *hx = "0123456789abcdef";
    for (int i = 0; i < 32; i++) { hex[i*2]= hx[digest[i]>>4]; hex[i*2+1]=hx[digest[i]&15]; }
    hex[64] = '\0'; return nv_str(hex);
}

// sha256_bytes(list-of-ints 0-255) → hex string
static inline NvVal nv_sha256_bytes(NvVal lst) {
    if (lst.tag != NV_LIST) return nv_str("");
    size_t n = (size_t)lst.list->len;
    uint8_t *buf = (uint8_t*)malloc(n);
    for (size_t i = 0; i < n; i++) buf[i] = (uint8_t)nv_to_i(lst.list->data[i]);
    uint8_t digest[32]; _nv_sha256(buf, n, digest); free(buf);
    char hex[65]; static const char *hx2 = "0123456789abcdef";
    for (int i = 0; i < 32; i++) { hex[i*2]= hx2[digest[i]>>4]; hex[i*2+1]=hx2[digest[i]&15]; }
    hex[64] = '\0'; return nv_str(hex);
}

// hmac_sha256(key, msg) → hex string
static inline NvVal nv_hmac_sha256(NvVal key, NvVal msg) {
    if (key.tag != NV_STR || msg.tag != NV_STR) return nv_str("");
    const char *k = key.s; const char *m = msg.s;
    size_t klen = strlen(k), mlen = strlen(m);
    uint8_t K[64] = {0};
    if (klen > 64) { _nv_sha256((const uint8_t*)k, klen, K); }
    else            { memcpy(K, k, klen); }
    uint8_t ipad[64+mlen], opad[64+32];
    for (int i = 0; i < 64; i++) { ipad[i] = K[i] ^ 0x36; opad[i] = K[i] ^ 0x5c; }
    memcpy(ipad + 64, m, mlen);
    uint8_t inner[32]; _nv_sha256(ipad, 64 + mlen, inner);
    memcpy(opad + 64, inner, 32);
    uint8_t digest[32]; _nv_sha256(opad, 64 + 32, digest);
    char hex[65]; static const char *hx3 = "0123456789abcdef";
    for (int i = 0; i < 32; i++) { hex[i*2]=hx3[digest[i]>>4]; hex[i*2+1]=hx3[digest[i]&15]; }
    hex[64] = '\0'; return nv_str(hex);
}

// ─────────────────────────────────────────────────────────────────────────────
// UTF-8 aware string operations
// ─────────────────────────────────────────────────────────────────────────────

// Count UTF-8 codepoints
static inline NvVal nv_utf8_len(NvVal s) {
    if (s.tag != NV_STR) return nv_int(0);
    const uint8_t *p = (const uint8_t*)s.s; int64_t n = 0;
    while (*p) { if ((*p & 0xC0) != 0x80) n++; p++; }
    return nv_int(n);
}

// UTF-8 codepoint at position idx (returns string of that codepoint)
static inline NvVal nv_utf8_at(NvVal s, NvVal idx) {
    if (s.tag != NV_STR) return nv_nil();
    const uint8_t *p = (const uint8_t*)s.s; int64_t want = nv_to_i(idx), n = 0;
    while (*p) {
        if ((*p & 0xC0) != 0x80) {
            if (n == want) {
                int bytes = (*p < 0x80) ? 1 : (*p < 0xE0) ? 2 : (*p < 0xF0) ? 3 : 4;
                char buf[5]; memcpy(buf, p, bytes); buf[bytes] = '\0';
                return nv_str(buf);
            }
            n++;
        }
        p++;
    }
    return nv_nil();
}

// UTF-8 codepoints as list of strings
static inline NvVal nv_utf8_chars(NvVal s) {
    NvVal lst = nv_list_new(); if (s.tag != NV_STR) return lst;
    const uint8_t *p = (const uint8_t*)s.s;
    while (*p) {
        if ((*p & 0xC0) != 0x80) {
            int bytes = (*p < 0x80) ? 1 : (*p < 0xE0) ? 2 : (*p < 0xF0) ? 3 : 4;
            char buf[5]; memcpy(buf, p, bytes); buf[bytes] = '\0';
            nv_list_push(lst, nv_str(buf));
        }
        p++;
    }
    return lst;
}

// ─────────────────────────────────────────────────────────────────────────────
// Subprocess: capture stdout of a shell command as a string
// ─────────────────────────────────────────────────────────────────────────────
static inline NvVal nv_popen_read(NvVal cmd) {
    if (cmd.tag != NV_STR) return nv_str("");
    FILE *f = popen(cmd.s, "r");
    if (!f) return nv_str("");
    char *buf = NULL; size_t cap = 0; size_t len = 0;
    char tmp[4096];
    while (fgets(tmp, sizeof(tmp), f)) {
        size_t tlen = strlen(tmp);
        if (len + tlen + 1 > cap) { cap = (cap + tlen + 1) * 2 + 4096; buf = (char*)realloc(buf, cap); }
        memcpy(buf + len, tmp, tlen); len += tlen;
    }
    pclose(f);
    if (!buf) return nv_str("");
    if (len > 0 && buf[len-1] == '\n') buf[--len] = '\0';
    else { buf = (char*)realloc(buf, len+1); buf[len] = '\0'; }
    NvVal v = nv_str(buf); free(buf); return v;
}

// ─────────────────────────────────────────────────────────────────────────────
// String formatting: sprintf-style (format, args...)
// ─────────────────────────────────────────────────────────────────────────────
// format_str(fmt, args_list) — fmt uses {0} {1} {2} placeholders
static inline NvVal nv_format_str(NvVal fmt, NvVal args) {
    if (fmt.tag != NV_STR) return fmt;
    NvVal lst = (args.tag == NV_LIST) ? args : nv_list_new();
    const char *f = fmt.s;
    char *out = (char*)malloc(1); out[0] = '\0'; size_t outlen = 0;
    #define _FMT_APPEND(s, n) do { size_t _n=(n); out=(char*)realloc(out,outlen+_n+1); memcpy(out+outlen,(s),_n); outlen+=_n; out[outlen]='\0'; } while(0)
    while (*f) {
        if (*f == '{' && *(f+1) >= '0' && *(f+1) <= '9') {
            const char *e = f+1; while (*e >= '0' && *e <= '9') e++;
            if (*e == '}') {
                int idx = atoi(f+1);
                NvVal v = (idx < lst.list->len) ? lst.list->data[idx] : nv_nil();
                NvVal sv = nv_to_str(v); const char *vs = sv.s;
                _FMT_APPEND(vs, strlen(vs));
                f = e+1; continue;
            }
        }
        _FMT_APPEND(f, 1); f++;
    }
    #undef _FMT_APPEND
    NvVal v = nv_str(out); free(out); return v;
}

// ─────────────────────────────────────────────────────────────────────────────
// Base64 encode / decode
// ─────────────────────────────────────────────────────────────────────────────
static inline NvVal nv_base64_encode(NvVal s) {
    if (s.tag != NV_STR) return nv_str("");
    static const char *b64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const uint8_t *in = (const uint8_t*)s.s; size_t len = strlen(s.s);
    size_t outlen = 4 * ((len + 2) / 3);
    char *out = (char*)malloc(outlen + 1); size_t j = 0;
    for (size_t i = 0; i < len; ) {
        uint32_t v = (uint32_t)in[i++] << 16;
        if (i < len) v |= (uint32_t)in[i++] << 8;
        if (i < len) v |= in[i++];
        out[j++] = b64[(v>>18)&63];
        out[j++] = b64[(v>>12)&63];
        out[j++] = b64[(v>>6)&63];
        out[j++] = b64[v&63];
    }
    if (len % 3 == 1) { out[outlen-1] = '='; out[outlen-2] = '='; }
    else if (len % 3 == 2) { out[outlen-1] = '='; }
    out[outlen] = '\0';
    NvVal r = nv_str(out); free(out); return r;
}

static inline NvVal nv_base64_decode(NvVal s) {
    if (s.tag != NV_STR) return nv_str("");
    static const int8_t dec[256] = {
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,62,-1,-1,-1,63,
        52,53,54,55,56,57,58,59,60,61,-1,-1,-1,-1,-1,-1,
        -1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,10,11,12,13,14,
        15,16,17,18,19,20,21,22,23,24,25,-1,-1,-1,-1,-1,
        -1,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,
        41,42,43,44,45,46,47,48,49,50,51,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,
        -1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1,-1
    };
    const uint8_t *in = (const uint8_t*)s.s; size_t inlen = strlen(s.s);
    if (inlen % 4) return nv_str("");
    size_t outlen = inlen/4*3;
    if (inlen > 0 && in[inlen-1]=='=') outlen--;
    if (inlen > 1 && in[inlen-2]=='=') outlen--;
    char *out = (char*)malloc(outlen + 1); size_t j2 = 0;
    for (size_t i2 = 0; i2 < inlen; i2 += 4) {
        uint32_t d0 = (uint32_t)(uint8_t)dec[in[i2]];
        uint32_t d1 = (uint32_t)(uint8_t)dec[in[i2+1]];
        uint32_t d2 = in[i2+2]=='=' ? 0u : (uint32_t)(uint8_t)dec[in[i2+2]];
        uint32_t d3 = in[i2+3]=='=' ? 0u : (uint32_t)(uint8_t)dec[in[i2+3]];
        uint32_t v2 = (d0<<18)|(d1<<12)|(d2<<6)|d3;
        out[j2++] = (uint8_t)(v2>>16);
        if (in[i2+2] != '=') out[j2++] = (uint8_t)(v2>>8);
        if (in[i2+3] != '=') out[j2++] = (uint8_t)v2;
    }
    out[j2] = '\0';
    NvVal rv = nv_str(out); free(out); return rv;
}

// ─────────────────────────────────────────────────────────────────────────────
// Tensor, trait, channel runtime (must come after all NvVal helpers)
// ─────────────────────────────────────────────────────────────────────────────
#include "nuvola_tensor.h"
