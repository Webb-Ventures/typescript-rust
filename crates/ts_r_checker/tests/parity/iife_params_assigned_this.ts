// Parameters of an immediately invoked function: typed by the arguments, rest parameters
// by the remaining ones, missing ones `undefined` and optional when unannotated.
((j?) => j + 1)(12);
((k?) => k + 1)();
((l, o?) => l + o)(12);
((n = 10) => n + 1)();
((...numbers) => numbers.every(n => n > 0))(5, 6, 7);
((...mixed) => mixed.every(n => !!n))(5, "oops", "oh no");
((...none) => none.some(n => n > 0))();
(function (x, undefined) { return x; })(42);
((x, y, z) => 42)();
((a: number, b) => a)();

// `this` in a function stored on an object is the object's type.
type Foo = any;
const foo: Foo = {};
foo.bar = function () {
    const self: Foo = this;
};
const target = { count: 0, inc() {} };
target.inc = function () {
    this.count++;
    this.missing;
};
const lit = {
    n: 1,
    f: function () { return this.n; },
};

export {};
