// Benchmark 2: Mandelbrot (C baseline)
#include <stdio.h>
int mandelbrot(double cx, double cy, int max_iter) {
    double x = 0.0, y = 0.0;
    for (int i = 0; i < max_iter; i++) {
        double x2 = x*x, y2 = y*y;
        if (x2 + y2 > 4.0) return i;
        y = 2.0*x*y + cy;
        x = x2 - y2 + cx;
    }
    return max_iter;
}
int main() {
    int size = 200;
    long total = 0;
    for (int row = 0; row < size; row++)
        for (int col = 0; col < size; col++) {
            double cx = (double)col/size * 3.5 - 2.5;
            double cy = (double)row/size * 2.0 - 1.0;
            total += mandelbrot(cx, cy, 1000);
        }
    printf("mandelbrot 200x200 total_iters = %ld\n", total);
    return 0;
}
