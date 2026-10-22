class C1 {
    declare Foo() { }
    declare get x() { return 1 }
}
declare enum E2 {
    x = "foo".length,
    y = 1 + 2,
    z = y,
    w = -1,
    v = `a`,
    u = E2.y,
}
type Key = string;
interface T {
    [key: Key]
}
class D { [k: string] }
const o: { [k: number] } = {};
class C2 {
    declare constructor() { }
    declare set x(v) { }
    declare static m() { }
    declare *g() { }
    declare async a() { }
}
const o2 = { declare m() {} };
