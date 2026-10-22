// @filename: types.ts
// Decorator metadata reads the named types as values: plain imports of types are TS1272.
export interface T {}
export class C {}
export type { C as TC };
export namespace NS { export interface I {} }
// @filename: index.ts
import { T, C, TC, NS } from "./types";
import type { T as TT } from "./types";
declare const dec: any;
@dec
class A {
    constructor(a: T, b: C, ...rest: TC[]) {}
    @dec m(x: T | null, y: NS.I): T { return undefined!; }
    @dec get g(): TT { return undefined!; }
    @dec set s(v: T | undefined) {}
    @dec p!: (T);
    @dec q!: T | C;
    @dec r!: string | T;
    method(@dec a: C, b: TC): T { return undefined!; }
}
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "experimentalDecorators": true, "emitDecoratorMetadata": true, "isolatedModules": true, "noEmit": true }, "files": ["types.ts", "index.ts"] }
