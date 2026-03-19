// Benchmark 1: Recursive Fibonacci (C baseline)
#include <stdio.h>
long fib(int n) {
    if (n <= 1) return n;
    return fib(n-1) + fib(n-2);
}
int main() {
    printf("fib(40) = %ld\n", fib(40));
    return 0;
}
