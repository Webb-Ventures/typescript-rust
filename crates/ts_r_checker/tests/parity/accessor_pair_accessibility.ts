class Base {
    get a() { return 0; }
    private set a(v) { }
    private get b() { return 0; }
    set b(v) { }
    protected set c(v) { }
    get c() { return 0; }
}
declare const x: Base;
x.a = 1;
void x.a;
x.a += 1;
x.b = 1;
void x.b;
x.b++;
x.c = 1;
void x.c;
