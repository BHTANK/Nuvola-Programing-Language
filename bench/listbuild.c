// Benchmark 9: List build + reduce — 1M elements (C baseline)
#include <stdio.h>
#include <stdlib.h>
int main() {
    int n = 1000000;
    long *arr = malloc(n * sizeof(long));
    for (int i = 0; i < n; i++) arr[i] = i;
    long sum = 0;
    for (int i = 0; i < n; i++) sum += arr[i];
    printf("list 1M build+sum = %ld\n", sum);
    free(arr);
    return 0;
}
