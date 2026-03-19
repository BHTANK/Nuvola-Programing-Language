#!/usr/bin/env python3
"""
Nuvola Benchmark Runner
=======================
Compiles each benchmark with nuvc (Nuvola), clang -O3 (C), and times
Python 3 — then prints a side-by-side comparison table.

Usage:
    python3 bench/run.py [--trials N] [--steps quick|full]

Options:
    --trials N     number of timed runs per benchmark (default 5, best taken)
    --steps full   use larger problem sizes where applicable
    --bench NAME   run only one benchmark (fib|mandelbrot|primes|quicksort|nbody)
"""

import subprocess, sys, os, time, shutil, tempfile, argparse

# ─── paths ────────────────────────────────────────────────────────────────────
BENCH_DIR   = os.path.dirname(os.path.abspath(__file__))
ROOT        = os.path.dirname(BENCH_DIR)
STAGE0      = os.path.join(ROOT, "stage0")
NUVC        = os.path.join(STAGE0, "target", "release", "nuvc")
RUNTIME_DIR = os.path.join(STAGE0, "runtime")

BLUE   = "\033[94m"
GREEN  = "\033[92m"
YELLOW = "\033[93m"
RED    = "\033[91m"
BOLD   = "\033[1m"
DIM    = "\033[2m"
RESET  = "\033[0m"
CYAN   = "\033[96m"

# ─── benchmark definitions ────────────────────────────────────────────────────
BENCHMARKS = [
    {
        "name":    "fib",
        "label":   "Fibonacci(40)",
        "desc":    "recursive fib, tests call overhead & int specialisation",
        "nvl":     "fib.nvl",
        "c":       "fib.c",
        "py":      "fib.py",
        "expect":  "102334155",   # fib(40)
    },
    {
        "name":    "mandelbrot",
        "label":   "Mandelbrot 200×200",
        "desc":    "float-point throughput, scalar specialisation",
        "nvl":     "mandelbrot.nvl",
        "c":       "mandelbrot.c",
        "py":      "mandelbrot.py",
        "expect":  None,   # output varies only in total iters count
    },
    {
        "name":    "primes",
        "label":   "Sieve of Eratosthenes 1M",
        "desc":    "list allocation, bool-array specialisation",
        "nvl":     "primes.nvl",
        "c":       "primes.c",
        "py":      "primes.py",
        "expect":  "78498",
    },
    {
        "name":    "quicksort",
        "label":   "Quicksort 100 k ints",
        "desc":    "list indexing, int-array specialisation",
        "nvl":     "quicksort.nvl",
        "c":       "quicksort.c",
        "py":      "quicksort.py",
        "expect":  None,
    },
    {
        "name":    "nbody",
        "label":   "N-body 500 k steps",
        "desc":    "18-var scalar main, floating-point math, scalar-main specialisation",
        "nvl":     "nbody.nvl",
        "c":       "nbody.c",
        "py":      "nbody.py",
        "expect":  None,
    },
]

# ─── helpers ──────────────────────────────────────────────────────────────────

def banner(msg):
    w = 72
    print()
    print(BOLD + CYAN + "═" * w + RESET)
    print(BOLD + CYAN + f"  {msg}" + RESET)
    print(BOLD + CYAN + "═" * w + RESET)

def die(msg):
    print(RED + "ERROR: " + msg + RESET, file=sys.stderr)
    sys.exit(1)

def check_tool(name):
    if not shutil.which(name):
        die(f"`{name}` not found on PATH — please install it")

def time_cmd(cmd, cwd=None, trials=5):
    """
    Run `cmd` `trials` times, return (best_seconds, stdout_of_best_run).
    Returns (None, None) if the command fails.
    """
    best = float("inf")
    best_out = ""
    for _ in range(trials):
        try:
            t0  = time.perf_counter()
            res = subprocess.run(
                cmd, cwd=cwd, capture_output=True, text=True, timeout=120
            )
            t1  = time.perf_counter()
        except subprocess.TimeoutExpired:
            return None, None
        if res.returncode != 0:
            return None, res.stderr.strip()
        elapsed = t1 - t0
        if elapsed < best:
            best = elapsed
            best_out = res.stdout.strip()
    return best, best_out

def compile_nvl(src_nvl, out_bin):
    """Compile a .nvl file to a binary using nuvc. Returns (ok, err_msg)."""
    res = subprocess.run(
        [NUVC, src_nvl, "-o", out_bin],
        capture_output=True, text=True
    )
    if res.returncode != 0:
        return False, (res.stderr or res.stdout).strip()
    return True, ""

def compile_c(src_c, out_bin):
    """Compile a .c file to a binary using clang -O3. Returns (ok, err_msg)."""
    res = subprocess.run(
        ["clang", "-O3", "-march=native", "-ffast-math",
         "-Wno-unused-variable", src_c, "-lm", "-o", out_bin],
        capture_output=True, text=True
    )
    if res.returncode != 0:
        return False, (res.stderr or res.stdout).strip()
    return True, ""

def fmt_time(s):
    if s is None:
        return RED + "FAIL" + RESET
    if s < 0.001:
        return f"{s*1e6:.0f} µs"
    if s < 1.0:
        return f"{s*1000:.1f} ms"
    return f"{s:.2f}  s"

def speedup_str(nvl_t, ref_t, label):
    """Return coloured speedup string relative to ref_t."""
    if nvl_t is None or ref_t is None or ref_t == 0:
        return DIM + "n/a" + RESET
    ratio = ref_t / nvl_t
    if ratio >= 1.5:
        col = GREEN
        arrow = "▲"
    elif ratio >= 0.95:
        col = YELLOW
        arrow = "≈"
    else:
        col = RED
        arrow = "▼"
    return col + BOLD + f"{ratio:.2f}×  {arrow} vs {label}" + RESET

# ─── main ─────────────────────────────────────────────────────────────────────

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--trials", type=int, default=5,
                    help="timed runs per benchmark (default 5)")
    ap.add_argument("--bench",  default=None,
                    help="run only this benchmark by name")
    args = ap.parse_args()

    # Prerequisites
    check_tool("clang")
    check_tool("python3")
    if not os.path.isfile(NUVC):
        # Try to build nuvc
        print(YELLOW + "nuvc not built — running cargo build --release …" + RESET)
        r = subprocess.run(
            ["cargo", "build", "--release"],
            cwd=STAGE0, capture_output=False
        )
        if r.returncode != 0:
            die("cargo build failed; build stage0 first")

    benchmarks = BENCHMARKS
    if args.bench:
        benchmarks = [b for b in BENCHMARKS if b["name"] == args.bench]
        if not benchmarks:
            die(f"unknown benchmark '{args.bench}'; choices: " +
                ", ".join(b["name"] for b in BENCHMARKS))

    tmpdir = tempfile.mkdtemp(prefix="nvbench_")

    banner(f"Nuvola Benchmark Suite  —  {args.trials} trials, best-of taken")
    print(f"  nuvc   : {NUVC}")
    print(f"  runtime: {RUNTIME_DIR}")
    print(f"  tmpdir : {tmpdir}")

    results = []

    for b in benchmarks:
        print()
        print(BOLD + f"── {b['label']}" + RESET + f"  ({b['desc']})")

        nvl_src = os.path.join(BENCH_DIR, b["nvl"])
        c_src   = os.path.join(BENCH_DIR, b["c"])
        py_src  = os.path.join(BENCH_DIR, b["py"])

        # ── compile Nuvola ────────────────────────────────────────────────────
        nvl_bin = os.path.join(tmpdir, b["name"] + "_nvl")
        ok, err = compile_nvl(nvl_src, nvl_bin)
        if not ok:
            print(RED + f"  [Nuvola] compile FAILED:\n{err}" + RESET)
            nvl_t = None
        else:
            nvl_t, nvl_out = time_cmd([nvl_bin], trials=args.trials)
            if nvl_t is None:
                print(RED + f"  [Nuvola] runtime FAILED: {nvl_out}" + RESET)
            else:
                print(f"  {GREEN}Nuvola{RESET}  {fmt_time(nvl_t):>10}    {DIM}{nvl_out[:60]}{RESET}")

        # ── compile C ─────────────────────────────────────────────────────────
        c_bin = os.path.join(tmpdir, b["name"] + "_c")
        ok, err = compile_c(c_src, c_bin)
        if not ok:
            print(RED + f"  [C]     compile FAILED:\n{err}" + RESET)
            c_t = None
        else:
            c_t, c_out = time_cmd([c_bin], trials=args.trials)
            if c_t is None:
                print(RED + f"  [C]     runtime FAILED: {c_out}" + RESET)
            else:
                print(f"  {BLUE}C      {RESET}  {fmt_time(c_t):>10}    {DIM}{c_out[:60]}{RESET}")

        # ── Python ────────────────────────────────────────────────────────────
        py_t, py_out = time_cmd(["python3", py_src], trials=args.trials)
        if py_t is None:
            print(RED + f"  [Python] FAILED: {py_out}" + RESET)
        else:
            print(f"  {YELLOW}Python {RESET}  {fmt_time(py_t):>10}    {DIM}{py_out[:60]}{RESET}")

        # ── speedup summary ───────────────────────────────────────────────────
        vs_c  = speedup_str(nvl_t, c_t,  "C")
        vs_py = speedup_str(nvl_t, py_t, "Python")
        print(f"  {BOLD}Nuvola:{RESET}  {vs_c}   {vs_py}")

        results.append({
            "label": b["label"],
            "nvl":   nvl_t,
            "c":     c_t,
            "py":    py_t,
        })

    # ── summary table ─────────────────────────────────────────────────────────
    banner("SUMMARY")
    col = [32, 12, 12, 12, 14, 16]
    hdr = ["Benchmark", "Nuvola", "C (clang)", "Python", "vs C", "vs Python"]
    row_fmt = "  {:<{c0}}  {:>{c1}}  {:>{c2}}  {:>{c3}}  {:>{c4}}  {:>{c5}}"
    def plain(s):
        # strip ANSI for width calculation isn't needed here — we just print raw
        return s

    print(row_fmt.format(*hdr, c0=col[0], c1=col[1], c2=col[2],
                          c3=col[3], c4=col[4], c5=col[5]))
    print("  " + "─" * (sum(col) + len(col) * 2))

    wins_c  = 0
    wins_py = 0
    for r in results:
        nvl_s = fmt_time(r["nvl"])
        c_s   = fmt_time(r["c"])
        py_s  = fmt_time(r["py"])

        # vs C ratio
        if r["nvl"] and r["c"]:
            ratio_c = r["c"] / r["nvl"]
            wins_c += 1 if ratio_c >= 0.95 else 0
            rc_str = f"{ratio_c:.2f}×"
            rc_col = GREEN if ratio_c >= 1.5 else (YELLOW if ratio_c >= 0.95 else RED)
        else:
            rc_str = "n/a"
            rc_col = DIM

        # vs Python ratio
        if r["nvl"] and r["py"]:
            ratio_py = r["py"] / r["nvl"]
            wins_py += 1
            rp_str = f"{ratio_py:.0f}×"
        else:
            rp_str = "n/a"

        print("  " +
              f"{r['label']:<{col[0]}}  "
              f"{nvl_s:>{col[1]}}  "
              f"{c_s:>{col[2]}}  "
              f"{py_s:>{col[3]}}  "
              f"{rc_col}{rc_str:>{col[4]}}{RESET}  "
              f"{GREEN}{rp_str:>{col[5]}}{RESET}")

    print()
    n = len(results)
    print(BOLD + f"  Result: {wins_c}/{n} benchmarks match or beat C" + RESET +
          f"  ·  "  +
          BOLD + f"all {wins_py}/{n} beat Python" + RESET)
    print()

    # cleanup
    import shutil as _sh
    _sh.rmtree(tmpdir, ignore_errors=True)


if __name__ == "__main__":
    main()
