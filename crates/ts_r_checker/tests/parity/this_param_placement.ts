// Misplaced `this` parameters: TS2680 not first, TS2681 constructors, TS2784 accessors.
interface Foo { n: number }
class C {
    n = 1;
    constructor(this: Foo) { }
    get x(this: Foo): number { return this.n; }
    set x(this: Foo, v: number) { }
}
interface I {
    new(this: Foo): I;
    get y(this: Foo): number;
}
declare var t: new (this: number) => number;
function notFirst(a: number, this: Foo): number { return a; }
type F = (a: number, this: Foo) => void;
export { };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
