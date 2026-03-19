import sys
sys.setrecursionlimit(200000)
def qs(arr, lo, hi):
    if lo >= hi: return
    pivot = arr[hi]
    i = lo
    for j in range(lo, hi):
        if arr[j] <= pivot:
            arr[i], arr[j] = arr[j], arr[i]
            i += 1
    arr[i], arr[hi] = arr[hi], arr[i]
    qs(arr, lo, i - 1)
    qs(arr, i + 1, hi)

n = 100000
arr = []
x = 12345
for _ in range(n):
    x = (x * 1664525 + 1013904223) % 4294967296
    arr.append(x % 1000000)
qs(arr, 0, n - 1)
print(f"sorted[0]={arr[0]} sorted[-1]={arr[-1]}")
