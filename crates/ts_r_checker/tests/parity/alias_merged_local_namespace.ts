// An import merged with a local value of its name (TS2440) is that local value in
// expressions, even when the import's target has a value too; its type meaning is the
// local namespace's, which has no `Bar`.
export namespace Library {
    export type Bar = { a: number };
}
// @filename: file2.ts
import * as Lib from './a';
namespace Lib {
    export const foo: string = "";
}
Lib.foo;
var x: Lib.Bar;
export { Lib };
