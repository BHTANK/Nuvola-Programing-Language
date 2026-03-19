#!/usr/bin/env bash
# ─────────────────────────────────────────────────────────────────────────────
# Nuvola Benchmark Runner — Shell Script
# Compiles + times: Nuvola (nuvc), C (clang -O3), Python 3
# Usage: bash bench/run.sh [--trials N] [--bench NAME]
# ─────────────────────────────────────────────────────────────────────────────
set -euo pipefail

BENCH_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$BENCH_DIR")"
STAGE0="$ROOT/stage0"
NUVC="$STAGE0/target/release/nuvc"
TRIALS=5
ONLY=""

# ── argument parsing ──────────────────────────────────────────────────────────
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

# ── helpers ───────────────────────────────────────────────────────────────────
die()    { echo -e "${RED}ERROR: $*${R}" >&2; exit 1; }
banner() { echo; printf "${BOLD}${CYAN}%s${R}\n" "$(printf '═%.0s' {1..72})"; \
           echo -e "${BOLD}${CYAN}  $*${R}"; \
           printf "${BOLD}${CYAN}%s${R}\n" "$(printf '═%.0s' {1..72})"; }

command -v clang   >/dev/null || die "clang not found"
command -v python3 >/dev/null || die "python3 not found"
[[ -f "$NUVC" ]] || { echo -e "${YELLOW}Building nuvc…${R}"; \
  (cd "$STAGE0" && cargo build --release --quiet); }
[[ -f "$NUVC" ]] || die "nuvc still not built"

TMPDIR_BIN="$(mktemp -d /tmp/nvbench_XXXXXX)"
trap 'rm -rf "$TMPDIR_BIN"' EXIT

# best_of N cmd... — sets ELAPSED (seconds, decimal) to best wall time
best_of() {
  local n="$1"; shift
  local best=99999
  for (( i=0; i<n; i++ )); do
    local t0 t1 elapsed
    t0=$(date +%s%N)
    "$@" >/dev/null 2>&1 || { ELAPSED="FAIL"; return; }
    t1=$(date +%s%N)
    elapsed=$(( (t1 - t0) ))          # nanoseconds
    (( elapsed < best )) && best=$elapsed
  done
  # convert ns → human string
  if   (( best < 1000000 ));    then ELAPSED="$(( best / 1000 )) µs"
  elif (( best < 1000000000 )); then ELAPSED="$(awk "BEGIN{printf \"%.1f ms\", $best/1e6}")"
  else                               ELAPSED="$(awk "BEGIN{printf \"%.2f s\",  $best/1e9}")"
  fi
  ELAPSED_NS=$best
}

fmt_ratio() {  # fmt_ratio nvl_ns ref_ns label
  local nvl=$1 ref=$2 lbl=$3
  [[ "$nvl" == "0" || "$ref" == "0" ]] && { echo -e "${DIM}n/a${R}"; return; }
  local ratio
  ratio=$(awk "BEGIN{printf \"%.2f\", $ref/$nvl}")
  local cmp
  cmp=$(awk "BEGIN{print ($ratio >= 1.5) ? \"fast\" : ($ratio >= 0.95) ? \"par\" : \"slow\"}")
  case "$cmp" in
    fast) echo -e "${GREEN}${BOLD}${ratio}× ▲ vs ${lbl}${R}" ;;
    par)  echo -e "${YELLOW}${BOLD}${ratio}× ≈ vs ${lbl}${R}" ;;
    slow) echo -e "${RED}${BOLD}${ratio}× ▼ vs ${lbl}${R}"   ;;
  esac
}

# ── benchmark list ────────────────────────────────────────────────────────────
declare -A LABELS DESCS
NAMES=("fib" "mandelbrot" "primes" "quicksort" "nbody")
LABELS=(
  [fib]="Fibonacci(40)"
  [mandelbrot]="Mandelbrot 200×200"
  [primes]="Sieve 1 M"
  [quicksort]="Quicksort 100 k"
  [nbody]="N-body 500 k"
)
DESCS=(
  [fib]="call overhead, int specialisation"
  [mandelbrot]="float throughput, scalar specialisation"
  [primes]="list alloc, bool-array specialisation"
  [quicksort]="list indexing, int-array specialisation"
  [nbody]="18-var scalar main, float math"
)

[[ -n "$ONLY" ]] && NAMES=("$ONLY")

banner "Nuvola Benchmark Suite  ·  ${TRIALS} trials, best-of taken"
echo -e "  nuvc   : $NUVC"
echo -e "  tmpdir : $TMPDIR_BIN"

# results accumulator: associative arrays
declare -A NVL_NS C_NS PY_NS NVL_STR C_STR PY_STR

for name in "${NAMES[@]}"; do
  echo
  echo -e "${BOLD}── ${LABELS[$name]}${R}  (${DESCS[$name]})"

  nvl_src="$BENCH_DIR/${name}.nvl"
  c_src="$BENCH_DIR/${name}.c"
  py_src="$BENCH_DIR/${name}.py"
  nvl_bin="$TMPDIR_BIN/${name}_nvl"
  c_bin="$TMPDIR_BIN/${name}_c"

  # ── Nuvola ─────────────────────────────────────────────────────────────────
  if "$NUVC" "$nvl_src" -o "$nvl_bin" 2>/dev/null; then
    best_of "$TRIALS" "$nvl_bin"
    NVL_NS[$name]="${ELAPSED_NS:-0}"
    NVL_STR[$name]="$ELAPSED"
    echo -e "  ${GREEN}Nuvola${R}   ${ELAPSED}"
  else
    NVL_NS[$name]=0; NVL_STR[$name]="${RED}COMPILE FAIL${R}"
    echo -e "  ${RED}Nuvola   COMPILE FAILED${R}"
  fi

  # ── C ──────────────────────────────────────────────────────────────────────
  if clang -O3 -march=native -ffast-math -Wno-unused-variable \
       "$c_src" -lm -o "$c_bin" 2>/dev/null; then
    best_of "$TRIALS" "$c_bin"
    C_NS[$name]="${ELAPSED_NS:-0}"
    C_STR[$name]="$ELAPSED"
    echo -e "  ${BLUE}C      ${R}   ${ELAPSED}"
  else
    C_NS[$name]=0; C_STR[$name]="${RED}COMPILE FAIL${R}"
    echo -e "  ${RED}C      COMPILE FAILED${R}"
  fi

  # ── Python ─────────────────────────────────────────────────────────────────
  best_of "$TRIALS" python3 "$py_src"
  PY_NS[$name]="${ELAPSED_NS:-0}"
  PY_STR[$name]="$ELAPSED"
  echo -e "  ${YELLOW}Python ${R}   ${ELAPSED}"

  # speedup
  echo -e "  ${BOLD}Speedup: $(fmt_ratio "${NVL_NS[$name]}" "${C_NS[$name]}"  "C")   $(fmt_ratio "${NVL_NS[$name]}" "${PY_NS[$name]}" "Python")${R}"
done

# ── summary table ─────────────────────────────────────────────────────────────
banner "SUMMARY"
printf "  %-22s  %12s  %12s  %12s  %10s  %12s\n" \
       "Benchmark" "Nuvola" "C (clang)" "Python" "vs C" "vs Python"
printf "  %s\n" "$(printf '─%.0s' {1..86})"

wins_c=0; wins_py=0; total=0
for name in "${NAMES[@]}"; do
  total=$(( total + 1 ))
  nvl_ns=${NVL_NS[$name]}; c_ns=${C_NS[$name]}; py_ns=${PY_NS[$name]}

  # vs C
  if (( nvl_ns > 0 && c_ns > 0 )); then
    rc=$(awk "BEGIN{printf \"%.2f×\", $c_ns/$nvl_ns}")
    ok=$(awk "BEGIN{print ($c_ns/$nvl_ns >= 0.95) ? 1 : 0}")
    (( ok )) && wins_c=$(( wins_c + 1 ))
    (( ok )) && rc_col="$GREEN" || rc_col="$RED"
  else
    rc="n/a"; rc_col="$DIM"
  fi

  # vs Python
  if (( nvl_ns > 0 && py_ns > 0 )); then
    rp=$(awk "BEGIN{printf \"%.0f×\", $py_ns/$nvl_ns}")
    wins_py=$(( wins_py + 1 ))
  else
    rp="n/a"
  fi

  printf "  %-22s  %12s  %12s  %12s  ${rc_col}%10s${R}  ${GREEN}%12s${R}\n" \
         "${LABELS[$name]}" "${NVL_STR[$name]}" "${C_STR[$name]}" "${PY_STR[$name]}" \
         "$rc" "$rp"
done

echo
echo -e "${BOLD}  Result: ${wins_c}/${total} match/beat C  ·  ${wins_py}/${total} beat Python${R}"
echo
