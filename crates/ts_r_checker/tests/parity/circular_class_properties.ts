// An unannotated class property whose initializer reads its own type through `this` is
// TS7022 at its name (identifier, private name or computed literal key).
class A {
    #foo = this.#bar;
    #bar = this.#foo;
    ["#baz"] = this["#baz"];
    a = this.b;
    b = this.a;
    ok = this.a ?? 1;
    fn = () => this.fn;
}
export {};
