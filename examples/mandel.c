#include <stdio.h>
#include <sgstat.h>

/* Direct port of the BASIC09 PROCEDURE mandel in the reference screenshot.
 * Keep its grid, constants, initial z=c, 16 updates, post-update escape test,
 * and character mapping. Double arithmetic replaces BASIC09 REAL.
 */
main()
{
    int x, y, i, pause;
    double ca, cb, a, b, t;
    struct sgbuf options;

    if (getstat(0, 1, &options) == -1)
        return 1;
    pause = options.sg_pause;
    options.sg_pause = 0;
    if (setstat(0, 1, &options) == -1)
        return 1;

    printf("Mandelbrot / BASIC09 to C / 6809\n");
    for (y = -12; y <= 12; y = y + 1) {
        for (x = -39; x <= 39; x = x + 1) {
            ca = x * 0.0458;
            cb = y * 0.08333;
            a = ca;
            b = cb;
            for (i = 0; i <= 15; i = i + 1) {
                t = a * a - b * b + ca;
                b = 2.0 * a * b + cb;
                a = t;
                if (a * a + b * b > 4.0)
                    break;
            }
            if (i > 15) {
                putchar(' ');
            } else {
                if (i > 9)
                    i = i + 7;
                putchar(48 + i);
            }
        }
        putchar('\n');
    }
    fflush(stdout);
    options.sg_pause = pause;
    if (setstat(0, 1, &options) == -1)
        return 1;
    printf("MANDELBROT-DONE\n");
    return 0;
}
