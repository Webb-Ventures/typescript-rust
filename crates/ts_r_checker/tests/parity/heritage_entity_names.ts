// Heritage elements of `implements` and interface `extends` must be entity names.
declare const ns: { I: new () => object } | undefined;
declare function f(): object;
interface A { }
namespace N { export interface B { } }
interface I1 extends A, N.B { }
interface I2 extends f() { }
interface I3 extends ns?.I { }
class C1 implements A, N.B { }
class C2 implements ns?.I { }
export { };
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "noEmit": true }, "files": ["a.ts"] }
