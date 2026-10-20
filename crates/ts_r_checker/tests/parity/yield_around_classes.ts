function* g() {
    class C extends (yield 0) { }
    var x = class D extends (yield) {};
    var y = class E {
        *[yield 0]() {
            yield 0;
        }
        [yield 1] = 2;
        z = { [yield 2]: 1 };
        static [yield 3]: number;
    };
}
