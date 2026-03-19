// Benchmark 7: Hash map stress — 500K insert + 500K lookup (C baseline)
// Uses a simple open-addressing hash table for fair comparison
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define CAPACITY 1048576  // power of 2, >2x 500K
#define N 500000

typedef struct { char key[20]; long val; int used; } Entry;
static Entry table[CAPACITY];

static unsigned hash(const char *s) {
    unsigned h = 5381;
    while (*s) h = h * 33 + (unsigned char)*s++;
    return h;
}

static void insert(const char *key, long val) {
    unsigned idx = hash(key) & (CAPACITY - 1);
    while (table[idx].used) {
        if (strcmp(table[idx].key, key) == 0) { table[idx].val = val; return; }
        idx = (idx + 1) & (CAPACITY - 1);
    }
    strcpy(table[idx].key, key);
    table[idx].val = val;
    table[idx].used = 1;
}

static long lookup(const char *key) {
    unsigned idx = hash(key) & (CAPACITY - 1);
    while (table[idx].used) {
        if (strcmp(table[idx].key, key) == 0) return table[idx].val;
        idx = (idx + 1) & (CAPACITY - 1);
    }
    return -1;
}

int main() {
    char buf[20];
    for (int i = 0; i < N; i++) {
        snprintf(buf, sizeof(buf), "key_%d", i);
        insert(buf, (long)i * 7);
    }
    long found = 0;
    for (int i = 0; i < N; i++) {
        snprintf(buf, sizeof(buf), "key_%d", i);
        if (lookup(buf) >= 0) found++;
    }
    printf("hashmap 500K insert+lookup, found=%ld\n", found);
    return 0;
}
