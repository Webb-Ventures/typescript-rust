export async function* test(x: Promise<number>) {
    enum E {
        foo = await x,
        baz = yield 1,
    }
    yield E.foo;
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "noImplicitAny": false, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
