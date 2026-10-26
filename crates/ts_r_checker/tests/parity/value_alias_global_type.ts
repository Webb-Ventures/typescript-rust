// An import of a value only does not match a type meaning, so a type reference to its
// name finds a global type of that name (fetch-blob's `import Blob from "./index.js"`);
// without one it is TS2749.
import { blobFrom, fooFrom, barFrom } from "./from";
export const x = blobFrom("a");
const y: number = fooFrom("a");
const z: number = x;
barFrom;
// @filename: blob.ts
export const Blob = class { b = 1 };
export default Blob;
// @filename: foo.ts
export const Foo = 1;
export default Foo;
// @filename: bar.ts
export const Bar = 1;
export default Bar;
// @filename: glob.d.ts
interface Foo { q: 1 }
interface Blob { size: number }
// @filename: from.ts
import Blob from "./blob";
import Foo from "./foo";
import Bar from "./bar";
export declare function blobFrom(path: string): Promise<Blob>;
export declare function fooFrom(path: string): Foo;
export declare function barFrom(path: string): Bar;
export { Blob };
