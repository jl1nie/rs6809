#include <stdio.h>
#include <sgstat.h>

/* K&R C, 16-bit int and 32-bit long on the 6809.
 * Fixed point: 1024 represents 1.0. No floating-point library required.
 * Only multiply z while |z| <= 2, so the long products stay in range.
 */
#define WIDTH 76
#define HEIGHT 25
#define LIMIT 48
#define SCALE 1024L

border()
{
    int x;
    putchar('+');
    for (x = 0; x < WIDTH; x = x + 1)
        putchar('-');
    putchar('+');
    putchar('\n');
}

main()
{
    int x, y, i, shade, pause;
    struct sgbuf options;
    long cr, ci, zr, zi, rr, ii;
    char *ramp;

    if (getstat(0, 1, &options) == -1) {
        printf("Cannot read terminal options\n");
        return 1;
    }
    pause = options.sg_pause;
    options.sg_pause = 0;
    if (setstat(0, 1, &options) == -1) {
        printf("Cannot disable terminal paging\n");
        return 1;
    }
    ramp = " .,:;ox%#@";
    printf("Mandelbrot / Microware C / 6809\n");
    printf("%d x %d characters, %d iterations\n", WIDTH, HEIGHT, LIMIT);
    border();
    for (y = 0; y < HEIGHT; y = y + 1) {
        ci = (y - 12) * 85L;
        putchar('|');
        for (x = 0; x < WIDTH; x = x + 1) {
            cr = -2176L + x * 42L;
            zr = 0L;
            zi = 0L;
            for (i = 0; i < LIMIT; i = i + 1) {
                rr = zr * zr / SCALE;
                ii = zi * zi / SCALE;
                if (rr + ii > 4L * SCALE)
                    break;
                zi = zr * zi / 512L + ci;
                zr = rr - ii + cr;
            }
            shade = i * 9 / LIMIT;
            putchar(ramp[shade]);
        }
        putchar('|');
        putchar('\n');
    }
    border();
    fflush(stdout);
    options.sg_pause = pause;
    if (setstat(0, 1, &options) == -1)
        return 1;
    printf("MANDELBROT-DONE\n");
    return 0;
}
