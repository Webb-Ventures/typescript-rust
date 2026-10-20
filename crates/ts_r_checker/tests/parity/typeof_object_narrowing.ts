// typeof narrowing: object types a primitive (or Function) is assignable to narrow to it;
// object members of an intersection with a primitive are tags, with a function the function.
declare function n(x: never): void;
function f1(x: { toString(): string } | undefined) { if (typeof x === "string") n(x); else n(x); }
function f2(x: { a: number }) { if (typeof x === "string") n(x); else n(x); }
function f3(x: { length: number }) { if (typeof x === "string") n(x); else n(x); }
function f4(x: string & { __h: true }) { if (typeof x === "string") n(x); else n(x); }
function f5(x: Object) { if (typeof x === "string") n(x); }
function f6(x: { toString(): string }) { if (typeof x === "number") n(x); else n(x); }
function f7<T extends { t: unknown }>(x: string & T) { if (typeof x === "string") n(x); else n(x); }
function f8(x: { length: number } | number) { if (typeof x === "object") n(x); else n(x); }
function f9(x: { length: number } | number) { if (typeof x === "function") n(x); else n(x); }
function f10(x: number & { __h: true } | string) { if (typeof x === "number") n(x); else n(x); }
function f11(x: { toFixed(): string } | string) { if (typeof x === "number") n(x); else n(x); }
function f12(x: { a: number } | string) { if (typeof x === "string") n(x); else n(x); }
function g1(x: (() => void) & { a: 1 }) { if (typeof x === "function") n(x); else n(x); }
function g2(x: (() => void) & { a: 1 }) { if (typeof x === "object") n(x); else n(x); }
function g3(x: { a: 1 } & { b: 1 }) { if (typeof x === "function") n(x); else n(x); }
function g4<T>(x: T & { b: 1 }) { if (typeof x === "string") n(x); else n(x); }
function g5(x: { toString(): string } | (() => void)) { if (typeof x === "function") n(x); else n(x); }
function g6(x: { toString(): string } | number) { if (typeof x === "symbol") n(x); else n(x); }
function g7(x: { toString(): string } | number) { if (typeof x === "undefined") n(x); else n(x); }
function g8(x: { length: number } | number) { if (typeof x !== "string") n(x); else n(x); }
declare const envVar: string | undefined;
if (typeof envVar === `string`) envVar.slice(0);
declare const obj: { test: string } | {};
if (`test` in obj) obj.test.slice(0);
