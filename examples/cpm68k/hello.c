/* Hello from Alcyon C on CP/M-68K (compile: SUBMIT C HELLO, SUBMIT CLINK HELLO) */
#include <stdio.h>

int sum(n)
int n;
{
	int i, s;

	s = 0;
	for (i = 1; i <= n; i++)
		s += i;
	return s;
}

main()
{
	printf("Hello from Alcyon C on CP/M-68K\n");
	printf("1+2+...+10 = %d\n", sum(10));
}
