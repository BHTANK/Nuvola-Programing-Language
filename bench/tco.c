// Benchmark 8: Integer arithmetic throughput — 10M ops (C baseline)
#include <stdio.h>
int main() {
    int n = 10000000;
    long sum = 0;
    for (int i = 0; i < n; i++) {
        long x = (long)i * 7 + 3;
        long y = x % 97;
        if (y > 50) sum += y; else sum -= y;
    }
    printf("intmath 10M ops, sum=%ld\n", sum);
    return 0;
}
