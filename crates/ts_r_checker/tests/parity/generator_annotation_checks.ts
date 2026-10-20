// A generator's annotation must accept the generator built from its own iteration types;
// `void` is TS2505. Yields (awaited in async generators) and what `yield*` operands iterate
// are checked against the annotated yield type.
export {};
interface WeirdIter extends IterableIterator<number> { hello: string; }
interface BadGenerator extends Iterator<number>, Iterable<string> { }
function* g1(): number { }
function* g2(): WeirdIter { }
function* g3(): BadGenerator { }
function* g4(): void { }
class Foo { x: number = 1 }
class Baz { z: number = 1 }
function* g5(): IterableIterator<Foo> {
    yield * [new Baz];
}
async function* a1(): AsyncIterableIterator<number> {
    yield "a";
    yield* ["a", "b"];
    yield* (async function* () { yield "a"; })();
}
async function* a2(): IterableIterator<number> {
    yield 1;
}
async function* a3(): Iterable<number> {
    yield 1;
}
