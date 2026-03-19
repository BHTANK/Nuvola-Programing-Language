def sieve(limit):
    composite = [False] * (limit + 1)
    p = 2
    while p * p <= limit:
        if not composite[p]:
            for m in range(p*p, limit+1, p):
                composite[m] = True
        p += 1
    return sum(1 for n in range(2, limit+1) if not composite[n])

print(f"primes below 1_000_000 = {sieve(1_000_000)}")
