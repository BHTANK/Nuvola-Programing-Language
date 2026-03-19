#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Nuvola vs C — Full Benchmark Suite
# Compiles + times: Nuvola (nuvc) vs C (clang -O3 -march=native -ffast-math)
# Usage: bash bench/run_full.sh [--trials N] [--bench NAME]
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$BENCH_DIR")"
NUVC="$ROOT/stage0/self/a.out"
TRIALS=5
ONLY=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --trials) TRIALS="$2"; shift 2 ;;
    --bench)  ONLY="$2";   shift 2 ;;
    *) echo "Unknown flag: $1"; exit 1 ;;
  esac
done

# ── colours ───────────────────────────────────────────────────────────────────
RED='\033[91m'; GREEN='\033[92m'; YELLOW='\033[93m'
BLUE='\033[94m'; CYAN='\033[96m'; BOLD='\033[1m'; DIM='\033[2m'; R='\033[0m'

banner() { echo; printf "${BOLD}${CYAN}%s${R}\n" "$(printf '═%.0s' {1..76})";
           echo -e "${BOLD}${CYAN}  $*${R}";
           printf "${BOLD}${CYAN}%s${R}\n" "$(printf '═%.0s' {1..76})"; }

command -v clang >/dev/null || { echo "clang not found"; exit 1; }
[[ -f "$NUVC" ]] || { echo "nuvc not found at $NUVC"; exit 1; }

TMPDIR_BIN="$(mktemp -d /tmp/nvbench_XXXXXX)"
trap 'rm -rf "$TMPDIR_BIN"' EXIT

# best_of N cmd... → sets ELAPSED (human), ELAPSED_NS (nanoseconds)
best_of() {
  local n="$1"; shift
  local best=999999999999
  for (( i=0; i<n; i++ )); do
    local t0 t1 elapsed
    t0=$(date +%s%N)
    "$@" >/dev/null 2>&1 || { ELAPSED="FAIL"; ELAPSED_NS=0; return; }
    t1=$(date +%s%N)
    elapsed=$(( t1 - t0 ))
    (( elapsed < best )) && best=$elapsed
  done
  ELAPSED_NS=$best
  if   (( best < 1000000 ));    then ELAPSED="$(( best / 1000 )) us"
  elif (( best < 1000000000 )); then ELAPSED="$(awk "BEGIN{printf \"%.1f ms\", $best/1e6}")"
  else                               ELAPSED="$(awk "BEGIN{printf \"%.3f s\",  $best/1e9}")"
  fi
}

# ── benchmark list ────────────────────────────────────────────────────────────
NAMES=("fib" "mandelbrot" "primes" "quicksort" "nbody" "matmul" "hashmap" "tco" "listbuild" "floatmath")
declare -A LABELS DESCS
LABELS=(
  [fib]="Fibonacci(40)"
  [mandelbrot]="Mandelbrot 200x200"
  [primes]="Sieve 1M"
  [quicksort]="Quicksort 100K"
  [nbody]="N-body 500K steps"
  [matmul]="Matrix 256x256"
  [hashmap]="HashMap 500K ops"
  [tco]="IntMath 10M ops"
  [listbuild]="List 1M build+sum"
  [floatmath]="FloatMath 2M iters"
)
DESCS=(
  [fib]="recursive call overhead, int specialization"
  [mandelbrot]="float throughput, scalar specialization"
  [primes]="list alloc, bool-array, sieve"
  [quicksort]="in-place partition, recursion, comparisons"
  [nbody]="18 scalar vars, float math, sqrt"
  [matmul]="triple nested loop, indexed array, float multiply-add"
  [hashmap]="string hashing, map insert+lookup, 500K keys"
  [tco]="integer multiply, mod, branch — 10M iterations"
  [listbuild]="dynamic array push 1M, sequential sum"
  [floatmath]="sin, cos, sqrt, exp, log — 2M calls"
)

[[ -n "$ONLY" ]] && NAMES=("$ONLY")

declare -A NVL_NS C_NS NVL_STR C_STR

banner "NUVOLA vs C  —  ${#NAMES[@]} BENCHMARKS  ·  ${TRIALS} trials, best-of"
echo -e "  ${GREEN}nuvc${R}  : $NUVC"
echo -e "  ${BLUE}clang${R} : $(clang --version | head -1)"
echo -e "  ${DIM}tmpdir : $TMPDIR_BIN${R}"
echo -e "  ${DIM}date   : $(date '+%Y-%m-%d %H:%M:%S')${R}"
echo -e "  ${DIM}kernel : $(uname -r)${R}"
echo -e "  ${DIM}cpu    : $(grep 'model name' /proc/cpuinfo | head -1 | cut -d: -f2 | xargs)${R}"

for name in "${NAMES[@]}"; do
  echo
  echo -e "${BOLD}── ${LABELS[$name]}${R}  ${DIM}(${DESCS[$name]})${R}"

  nvl_src="$BENCH_DIR/${name}.nvl"
  c_src="$BENCH_DIR/${name}.c"
  nvl_bin="$TMPDIR_BIN/${name}_nvl"
  c_bin="$TMPDIR_BIN/${name}_c"

  # ── Compile Nuvola ──
  nvl_compile_t0=$(date +%s%N)
  if "$NUVC" "$nvl_src" -o "$nvl_bin" 2>/dev/null; then
    nvl_compile_t1=$(date +%s%N)
    nvl_compile_ms=$(awk "BEGIN{printf \"%.0f\", ($nvl_compile_t1-$nvl_compile_t0)/1e6}")
    echo -e "  ${DIM}compile: nuvc ${nvl_compile_ms}ms${R}"
  else
    NVL_NS[$name]=0; NVL_STR[$name]="COMPILE FAIL"
    echo -e "  ${RED}Nuvola   COMPILE FAILED${R}"
    continue
  fi

  # ── Compile C ──
  c_compile_t0=$(date +%s%N)
  clang -O3 -march=native -ffast-math -Wno-unused-variable \
       "$c_src" -lm -o "$c_bin" 2>/dev/null
  c_compile_t1=$(date +%s%N)
  c_compile_ms=$(awk "BEGIN{printf \"%.0f\", ($c_compile_t1-$c_compile_t0)/1e6}")
  echo -e "  ${DIM}compile: clang ${c_compile_ms}ms${R}"

  # ── Run Nuvola ──
  best_of "$TRIALS" "$nvl_bin"
  NVL_NS[$name]="${ELAPSED_NS}"
  NVL_STR[$name]="$ELAPSED"
  echo -e "  ${GREEN}Nuvola${R}  ${BOLD}${ELAPSED}${R}"

  # ── Run C ──
  best_of "$TRIALS" "$c_bin"
  C_NS[$name]="${ELAPSED_NS}"
  C_STR[$name]="$ELAPSED"
  echo -e "  ${BLUE}C     ${R}  ${BOLD}${ELAPSED}${R}"

  # ── Ratio ──
  nvl=${NVL_NS[$name]}; cns=${C_NS[$name]}
  if (( nvl > 0 && cns > 0 )); then
    ratio=$(awk "BEGIN{printf \"%.2f\", $cns/$nvl}")
    pct=$(awk "BEGIN{printf \"%.0f\", (1-$cns/$nvl)*100}")
    if (( $(awk "BEGIN{print ($cns/$nvl >= 0.90) ? 1 : 0}") )); then
      echo -e "  ${GREEN}${BOLD}→ Nuvola is ${ratio}x C speed (${pct}% overhead)${R}"
    elif (( $(awk "BEGIN{print ($cns/$nvl >= 0.50) ? 1 : 0}") )); then
      echo -e "  ${YELLOW}${BOLD}→ Nuvola is ${ratio}x C speed${R}"
    else
      echo -e "  ${RED}${BOLD}→ Nuvola is ${ratio}x C speed${R}"
    fi
  fi
done

# ── SUMMARY TABLE ─────────────────────────────────────────────────────────────
banner "RESULTS SUMMARY — Nuvola vs C (clang -O3 -march=native -ffast-math)"
printf "  ${BOLD}%-24s  %14s  %14s  %10s  %s${R}\n" \
       "Benchmark" "Nuvola" "C (clang)" "Ratio" "Verdict"
printf "  %s\n" "$(printf '─%.0s' {1..80})"

wins=0; ties=0; total=0
for name in "${NAMES[@]}"; do
  total=$(( total + 1 ))
  nvl_ns=${NVL_NS[$name]:-0}; c_ns=${C_NS[$name]:-0}

  verdict=""
  ratio_str="n/a"
  if (( nvl_ns > 0 && c_ns > 0 )); then
    ratio=$(awk "BEGIN{r=$c_ns/$nvl_ns; printf \"%.2f\", r}")
    ratio_str="${ratio}x"
    verdict_code=$(awk "BEGIN{r=$c_ns/$nvl_ns; print (r>=1.0)?\"WIN\":(r>=0.8)?\"CLOSE\":(r>=0.5)?\"OK\":\"SLOW\"}")
    case "$verdict_code" in
      WIN)   verdict="${GREEN}${BOLD}BEATS C${R}"; wins=$((wins+1)) ;;
      CLOSE) verdict="${YELLOW}${BOLD}~C SPEED${R}"; ties=$((ties+1)) ;;
      OK)    verdict="${YELLOW}OK${R}" ;;
      SLOW)  verdict="${RED}slower${R}" ;;
    esac
  fi

  printf "  %-24s  %14s  %14s  %10s  %b\n" \
         "${LABELS[$name]}" "${NVL_STR[$name]}" "${C_STR[$name]}" \
         "$ratio_str" "$verdict"
done

printf "  %s\n" "$(printf '─%.0s' {1..80})"
echo -e "  ${BOLD}Nuvola BEATS C:${R}   ${GREEN}${BOLD}$wins / $total${R}"
echo -e "  ${BOLD}Within 20% of C:${R} ${YELLOW}${BOLD}$ties / $total${R}"
echo -e "  ${BOLD}Total benchmarks:${R} $total"
echo
