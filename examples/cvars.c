#include <stdio.h>

/* Variable display test for the ICE (Microware C, K&R). */
int gcount;
char gflag;
long gbig;
int garr[4];
static int scount;
char *gname = "cvars";

struct pt {
    char tag;
    int x;
    long y;
};

typedef struct node {
    int val;
    struct node *next;
} NODE;

int locals(p, n)
int *p;
int n;
{
    char c1;
    int i;
    long l;
    char c2;
    int arr[3];
    struct pt s;
    register int k;
    c1 = 'A';
    i = 0x1234;
    l = 0x11223344;
    c2 = 'B';
    arr[0] = 1; arr[1] = 2; arr[2] = 3;
    s.tag = 'T'; s.x = 0x55AA; s.y = 0x0102;
    k = n;
    for (i = 0; i < n; i++) {
        int inner;
        inner = p[i] * 2;
        k += inner;
    }
    return k + c1 + c2 + arr[1];
}

int regs(n, m)
register int n;
int m;
{
    register int a;
    register char *q;
    unsigned u;
    short sh;
    char buf[5];
    NODE nd, nd2;
    a = n + m;
    u = 40000;
    sh = -3;
    buf[0] = 'o'; buf[1] = 'k'; buf[2] = 0;
    nd2.val = 99;
    nd2.next = 0;
    nd.val = a;
    nd.next = &nd2;
    q = buf;
    {
        long y1;
        y1 = a;
        a = (int)y1 + sh + nd.next->val;
    }
    return a + u;
}

main()
{
    int v[2];
    static int calls;
    v[0] = 7; v[1] = 9;
    gcount = 3; gflag = 'g'; gbig = 70000; garr[2] = 42; scount = 5;
    calls++;
    printf("cvars %d\n", locals(v, 2));
    printf("cvars %d\n", regs(1, 2));
    return 0;
}
