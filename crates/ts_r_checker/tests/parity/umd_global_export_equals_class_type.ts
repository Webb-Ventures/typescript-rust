// A UMD global (`export as namespace Foo`) whose `export =` is a class merged with a
// namespace is that class in type position, and the namespace in qualified names.
/// <reference path="foo.d.ts" />
import * as ff from './foo';
declare let y: Foo;
const n: string = y.foo();
declare let z: Foo.SubThing;
let x: any = Foo;
// @filename: foo.d.ts
declare class Thing {
    foo(): number;
}
declare namespace Thing {
    interface SubThing { }
}
export = Thing;
export as namespace Foo;
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "commonjs", "noEmit": true }, "files": ["a.ts"] }
