def mandelbrot(cx, cy, max_iter):
    x = y = 0.0
    for i in range(max_iter):
        x2, y2 = x*x, y*y
        if x2 + y2 > 4.0: return i
        y = 2.0*x*y + cy
        x = x2 - y2 + cx
    return max_iter

size = 200
total = 0
for row in range(size):
    for col in range(size):
        cx = col/size * 3.5 - 2.5
        cy = row/size * 2.0 - 1.0
        total += mandelbrot(cx, cy, 1000)
print(f"mandelbrot 200x200 total_iters = {total}")
