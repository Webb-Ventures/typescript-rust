function* g() {
    class A {
        x = yield 1;
        static y = yield 1;
        static { const a = yield 1; }
        m() { const b = yield 1; }
    }
    const f = () => { const c = yield 1; };
    function h() { const d = yield 1; }
    const o = { p: function () { const q = yield 1; } };
}
function k() { const e = yield 1; }
function* keys() {
    let a = { [yield 0]: 0 };
    let b = { [yield 1]() { } };
    let c = { get [yield 2]() { return 0; } };
    let d = { set [yield 3](v: number) { } };
    let e = { *[yield 4]() { } };
    let f = { async [yield 5]() { } };
    class C { [yield 6]() { } }
    class D { get [yield 7]() { return 0; } }
    let h = { x: 1, [yield 8]() { } };
}
function* d1() {
    const [a = 1, b = 2] = yield;
}
function* d3() {
    const { c, d } = yield;
}
function* d4() {
    let x, y;
    [x, y] = yield;
}
function* d5() {
    const { e = 1 } = yield;
}
function* d6([p, q] = yield) {
}
