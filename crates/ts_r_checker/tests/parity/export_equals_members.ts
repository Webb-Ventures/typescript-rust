// `export =` of a class merged with a namespace: named imports are the namespace's exports, a
// default import (esModuleInterop) is the entity, a missing member of a pure namespace is
// TS2305, and a value-only `export =` has no namespace meaning (TS2503).
import Container, { ContainerProps, Kind } from './container'
import { Missing } from './ns'
import v = require('./val')
export declare const p: ContainerProps
export declare const c: Container
export declare const k: Kind
const C2 = Container
export const made = new C2()
let a: v.A
// @filename: container.d.ts
declare namespace Container {
  export interface ContainerProps { a: number }
  export type Kind = "x"
}
declare class Container<C = string> {
  static default: "foo";
  x: C
}
export = Container
// @filename: ns.d.ts
declare namespace Ns { function f(): void }
export = Ns
// @filename: val.ts
var x = 1;
export = x;
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "noEmit": true, "module": "commonjs", "target": "es2015", "esModuleInterop": true}, "files": ["a.ts", "container.d.ts", "ns.d.ts", "val.ts"]}
