interface b {}
export const b = 1;
type t = 0;
namespace t { interface I {} }
export const t = 0;
interface c {}
export interface c {}
interface d {}
export class d {}
namespace N { }
export namespace N {}
var f = 10;
export function f(): void;
export function f() {}
function g() {}
export namespace g { export const x = 1; }
class K {}
export namespace K { export const y = 1; }
namespace NI { }
export function NI() {}
enum E { A }
export enum E { B = 2 }
export namespace Outer {
    interface I {}
    export interface I {}
    function h() {}
    export namespace h {}
}
declare namespace D {
    interface J {}
    export interface J {}
}
export type TA = {};
type TA = {};
interface TI {}
export type TI = {};
namespace EN { export enum M { A } }
import EM = EN.M;
export type EM = 1;
var ov = 1;
export function ov(): void;
export function ov() {}
function fo(): void;
export function fo(x: number): void;
function fo(x?: number) {}
