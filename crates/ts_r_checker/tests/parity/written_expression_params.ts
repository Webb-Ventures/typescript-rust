// Parameters of function and arrow expressions print as annotated (literal quotes,
// `Array<T>`, aliases), unless the annotation names an enclosing type parameter.
export {};
const a = (x: 'bye', y: Array<string>, z: 1 | "a") => 1;
const n1: number = a;
const b = function (x: Array<number>, ...r: Array<string>) { };
const n2: number = b;
function g<T>(t: T) { return (x: Array<T>, y: T) => 1; }
const n3: number = g(1);
const n4: number = g;
type Al = Array<number>;
const c = (x: Al, y: Readonly<{ a: 1 }>) => 1;
const n5: number = c;
declare function take(f: (x: "hi") => number): void;
take((x: 'bye') => 1);
const o = { m: (x: Array<string>) => 1 };
const n6: number = o;
