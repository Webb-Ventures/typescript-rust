class P<E, A> implements PromiseLike<A> {
    declare readonly e: E;
    constructor(readonly p: PromiseLike<A>) { }
    then<B = A, B1 = never>(
        this: P<never, A>,
        onfulfilled?: ((value: A) => B | PromiseLike<B>) | null | undefined,
        onrejected?: ((reason: any) => B1 | PromiseLike<B1>) | null | undefined
    ): PromiseLike<B | B1> {
        return this.p.then(onfulfilled, onrejected);
    }
}
declare const failing: P<number, string>;
async function test() {
    await failing;
}
const obj = {
    [Symbol.asyncIterator]() {
        return { next() { return { then() { } }; } };
    }
};
async function* g() {
    yield* obj;
}
var x = { [x]: 1 };
var c = { g: c.f() };
export { test, g };
// @filename: tsconfig.json
{"compilerOptions":{"strict":true,"target":"ES2023","lib":["ES2023","ES2018.AsyncIterable"],"module":"ESNext","moduleResolution":"Bundler","moduleDetection":"force","noEmit":true},"files":["a.ts"]}
