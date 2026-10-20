// A bare `yield` yields `undefined`; `yield* x` contextually types `x` as an iterable of the
// yield type, and an immediately invoked generator yields into its call's context. `yield`
// in a class's heritage clause belongs to the enclosing generator.
export {};
class Foo { x: number = 1 }
function* k(): IterableIterator<Foo> {
    yield;
}
function* g(): IterableIterator<(x: string) => number, (x: string) => number> {
    yield x => x.length;
    yield *[x => x.length];
    return x => x.length;
}
function* h(): IterableIterator<(x: string) => number> {
    yield * function* () {
        yield x => x.length;
    } ();
}
function* m(): Generator<any, void, any> {
    class C extends (yield 0) { }
    var y = class E { p = yield 1; };
}
async function* a(): AsyncGenerator<(x: string) => number> {
    yield* [x => x.length];
}
