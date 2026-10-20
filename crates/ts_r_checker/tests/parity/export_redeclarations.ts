// Exports of a module declared more than once: TS2323 at each declaration.
export var a = 1;
export var a = 2;
export declare var b: number;
export declare var b: number;
export var c = 3, c = 4;
export var { d1, d1: d2 } = { d1: 1 };
export var d1: number;

// Overloads, namespaces, class + interface and interfaces alone are fine.
export function f(): void;
export function f(x?: number) {}
export namespace N { export var z = 1; }
export namespace N { export var y = 2; }
export class C {}
export interface C {}
export interface I {}
export interface I {}
var loc = 1;
var loc = 2;
export { loc };

// Default exports: the interface is reported too, the functions also get TS2393.
export default interface A { a: string; }
export default function () { return 1; }
export default function () { return 2; }
