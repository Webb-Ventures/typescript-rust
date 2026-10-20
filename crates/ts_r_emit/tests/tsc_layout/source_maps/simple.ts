export let x = 1;
function f(a: number, b) {
    if (a > b) {
        return a + b;
    }
    for (let i = 0; i < 3; i++) f(i, x);
    return foo.bar(a, [1, 2], { k: 3, m });
}
class C extends D {
    y = 2;
    m(p) { return this.y; }
}
