// `export = X` names a later class without reading it early; `export default X` reads it.
export = Foo;
class Foo<A> {
    constructor(public a: A) { }
}
// @filename: b.ts
export default Bar;
class Bar {}
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "commonjs",
        "moduleDetection": "force", "noEmit": true
    },
    "files": ["a.ts", "b.ts"]
}
