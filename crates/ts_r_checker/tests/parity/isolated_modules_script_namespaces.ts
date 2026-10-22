// @filename: script.ts
// Under isolatedModules, namespaces and enums of global scripts merge across files.
namespace Values { export const x = 1; }
namespace Types { export type T = number; }
namespace Outer.Inner { export const y = 2; }
namespace WithConst { export const enum K { A } }
declare namespace Ambient { export const z: number; }
// @filename: enum1.ts
enum Enum { A, B }
declare enum Enum { X = 1_000 }
// @filename: enum2.ts
enum Enum {
    C = A,
    D = X,
    E = Enum.B,
}
declare enum Enum {
    F = A,
}
// @filename: mod.ts
export namespace M { export const w = 3; }
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2020", "module": "esnext", "isolatedModules": true, "noEmit": true }, "files": ["script.ts", "enum1.ts", "enum2.ts", "mod.ts"] }
