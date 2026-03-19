// Benchmark 3: Sieve of Eratosthenes (C baseline)
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
int main() {
    int limit = 1000000;
    char *composite = calloc(limit+1, 1);
    for (int p = 2; (long)p*p <= limit; p++)
        if (!composite[p])
            for (int m = p*p; m <= limit; m += p)
                composite[m] = 1;
    int count = 0;
    for (int n = 2; n <= limit; n++)
        if (!composite[n]) count++;
    printf("primes below 1_000_000 = %d\n", count);
    free(composite);
    return 0;
}
