// Benchmark 4: Quicksort (C baseline)
#include <stdio.h>
#include <stdlib.h>
void qs(long *a, int lo, int hi) {
    if (lo >= hi) return;
    long pivot = a[hi], tmp;
    int i = lo, j = lo;
    while (j < hi) {
        if (a[j] <= pivot) { tmp=a[i]; a[i]=a[j]; a[j]=tmp; i++; }
        j++;
    }
    tmp=a[i]; a[i]=a[hi]; a[hi]=tmp;
    qs(a, lo, i-1); qs(a, i+1, hi);
}
int main() {
    int n = 100000;
    long *arr = malloc(n * sizeof(long));
    long x = 12345;
    for (int i = 0; i < n; i++) {
        x = (x * 1664525L + 1013904223L) % 4294967296L;
        arr[i] = x % 1000000;
    }
    qs(arr, 0, n-1);
    printf("sorted[0]=%ld sorted[-1]=%ld\n", arr[0], arr[n-1]);
    free(arr);
    return 0;
}
