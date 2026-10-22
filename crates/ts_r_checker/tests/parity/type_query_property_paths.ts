interface DeepOptional { a?: { b?: { c?: string } } }
function init2(foo: DeepOptional) {
    type Z = typeof foo.a.b;
    if (foo.a) {
        type A = typeof foo.a;
        type B = typeof foo.a.b;
        type C = typeof foo.a.b.c;
        for (const _ of [1]) {
            type C2 = typeof foo.a.b.c;
        }
        if (foo.a.b) {
            type C3 = typeof foo.a.b.c;
        }
    }
}
