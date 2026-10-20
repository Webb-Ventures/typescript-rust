// An any key on a type without index signatures is implicitly any (TS7053), shown
// as the apparent type: '{}' for object, 'unknown' for an unconstrained type parameter.
declare const k: any;
interface I { a: number }
function g<T extends object>(t: T) {
  return t[k];
}
function h<T>(t: T) {
  return t[k];
}
function i<T extends I>(t: T) {
  return t[k];
}
function j<T extends Record<string, number>>(t: T) {
  return t[k];
}
function m<T extends { a: 1 } | { b: 2 }>(t: T) {
  return t[k];
}
declare const u: { a: 1 } | { b: 2 };
const u1 = u[k];
declare const n: number;
const n1 = n[k];
declare const s: symbol;
const s1 = s[k];
declare const ob: object;
const o1 = ob[k];
declare const ix: I & { b: string };
const x1 = ix[k];
declare const fn: () => void;
const f1 = fn[k];
function q<K extends keyof I>(t: I[K]) {
  return t[k];
}
