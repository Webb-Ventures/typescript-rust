// Type parameter constraints print as written (`Array<string>`, aliases, union order),
// and an instantiated `Array<U>` keeps its form. Union reduction does not treat a
// primitive as a subtype of `Object` (it does `{}`); inference does.
export {};
declare let a: <T extends Array<string>>(x: T) => T;
const n1: number = a;
declare let b: <T extends ReadonlyArray<string>>(x: T) => T;
const n2: number = b;
declare let c: <T extends {a:string}>(x: T) => T;
const n3: number = c;
type Al = Array<number>;
declare let d: <T extends Al>(x: T) => T;
const n4: number = d;
declare let f: <T extends Array<string> | number>(x: T) => T;
const n6: number = f;
declare let g: <T extends Array<string>>(x: Array<T>) => Array<T>;
const n7: number = g;
declare class C<U> { m<T extends Array<U>>(x: T): T; k<T extends Array<string>>(x: U): T; }
const n11: number = new C<number>().m;
const n12: number = new C<number>().k;
namespace NS { export interface Q {} }
declare let q: <T extends NS.Q>(x: T) => T;
const n13: number = q;

var r6 = true ? (x: number) => { } : (x: Object) => { };
r6.foo;
var r9 = true ? (x: {}) => { } : (x: number) => { };
r9.foo;
declare let fa: (x: Object) => void;
declare let fb: (x: number) => void;
var r11 = [fa, fb];
r11.foo;
declare function f1<T>(f1: (x: T) => void, f2: (x: T) => void): (x: T) => void;
declare function fo(x: Object): void;
declare function fs(x: string): void;
const x1: number = f1(fo, fs);
