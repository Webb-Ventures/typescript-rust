// `any` and `never` indexed by a type that cannot be a key are TS2538.
var x: any[[]];
type A1 = any[boolean];
type A2 = any[{}];
type A3 = any[string | boolean];
type A4 = any["a" | 1];
type A5 = any[undefined];
type A6 = never[[]];
type A7 = any[null];
