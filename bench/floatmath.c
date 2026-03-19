// Benchmark 10: Heavy floating-point math — 2M iterations (C baseline)
#include <stdio.h>
#include <math.h>
int main() {
    int n = 2000000;
    double sum = 0.0;
    for (int i = 0; i < n; i++) {
        double x = i * 0.000001;
        sum += sin(x) * cos(x) + sqrt(x + 1.0) + log(x + 1.0);
    }
    printf("floatmath 2M iters, sum=%g\n", sum);
    return 0;
}
