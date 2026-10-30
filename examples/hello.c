#include <stdio.h>

/* Microware C uses K&R syntax and 16-bit int. */
int sum(n)
int n;
{
    int total;
    total = 0;
    while (n > 0) {
        total = total + n;
        n = n - 1;
    }
    return total;
}

main()
{
    int answer;
    answer = sum(6) * 2;
    printf("Hello from C: %d\n", answer);
    return 0;
}
