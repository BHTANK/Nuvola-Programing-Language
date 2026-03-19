// Benchmark 6: Matrix multiplication 256x256 (C baseline)
#include <stdio.h>
#define N 256
static double a[N*N], b[N*N], c[N*N];
int main() {
    for (int i = 0; i < N*N; i++) {
        a[i] = (i % 97) * 0.01;
        b[i] = (i % 53) * 0.01;
        c[i] = 0.0;
    }
    for (int row = 0; row < N; row++)
        for (int col = 0; col < N; col++) {
            double sum = 0.0;
            for (int k = 0; k < N; k++)
                sum += a[row*N+k] * b[k*N+col];
            c[row*N+col] = sum;
        }
    printf("matmul 256x256 c[0]=%g c[last]=%g\n", c[0], c[N*N-1]);
    return 0;
}
