// A type literal in a generic signature that does not name a type parameter is not
// instantiated by it, so it doesn't fix that parameter before a callback infers it.
interface El { isConnected: boolean }
type D = { elements: El[] };
declare function call6<R>(f: (data: { elements: El[] }) => R): R;
declare function call7<R>(f: (data: D) => R): R;
declare function call8<R>(f: (data: { elements: number }) => R): R;
declare function call9<R>(f: (data: El) => R): R;
const r1 = call6((data: { elements: El[] }) => data.elements);
const r2 = call7((data) => data.elements);
const r3 = call8((data) => data.elements);
const r4 = call9((data) => data.isConnected);
const r5 = call6((data) => 1);
const n1: number = r1;
const n2: number = r2;
const n3: string = r3;
const n4: number = r4;
const n5: string = r5;
