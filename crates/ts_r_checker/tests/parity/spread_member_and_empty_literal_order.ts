declare const u: { a: number } | undefined;
declare const n: { b: number } | null;
var o1: {} | { a: number } | { b: number } | { a: number, b: number };
var o1 = { ...n, ...u };
