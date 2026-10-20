namespace m { export var v = ''; export interface I {} export namespace NI { export interface J {} } export namespace NV { export var z = 1; } export class C {} export enum E { A } export type T = number; }
var a1 = ''; import a1 = m.v;
import a2 = m.v; var a2 = '';
interface a3 {} import a3 = m.v;
import a4 = m.I; var a4 = 1;
import a5 = m.I; interface a5 {}
import a6 = m.NI; namespace a6 { export interface K {} }
import a7 = m.NV; namespace a7 { export interface K {} }
function a8() {} import a8 = m.v;
import a9 = m.C; interface a9 {}
import a10 = m.T; var a10 = 1;
import a11 = m.T; type a11 = string;
import a12 = m.E; namespace a12 { export var q = 1; }
namespace P { var q = 1; }
namespace P { export import q = m.v; }
namespace Q { export var q = 1; }
namespace Q { import q = m.v; }
