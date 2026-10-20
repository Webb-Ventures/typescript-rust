// One modifier error per declaration, at the first keyword that breaks a rule, in the
// order the keywords are written.
class B1 {
    declare export p1: number;
    static export p2 = 1;
    async export m1() {}
    public export p5 = 1;
    readonly async m3() {}
    async readonly m4() {}
    async static get g1() { return 1; }
    static async get g2() { return 1; }
    async declare p6: number;
    declare async p7: number;
    declare readonly m6(): void;
    private async constructor() {}
    async private m8() {}
    accessor accessor p8 = 1;
    readonly static p10 = 1;
    declare #p12(): void;
    protected #p14() {}
    readonly static [k: symbol]: number;
    async [k: `a${string}`]: number;
    accessor q?: number = 1;
    async *gen() {}
}
declare class B2 {
    declare p: number;
    declare m(): void;
    async m2(): Promise<void>;
}
declare namespace N3 {
    async function f(): Promise<void>;
    namespace Inner {
        declare var x: number;
    }
}
declare global {
    declare var gx: number;
}
async declare function f6(): Promise<void>;
declare export async function f8(): Promise<void>;
static export function f10() {}
abstract export class F13 {}
async class A1 {}
async enum A2 { X }
readonly function rf() {}
abstract interface AI {}
public class PC {}
accessor function acf() {}
class B3 {
    constructor(static x: number, export y: number, readonly readonly z: number, public private w: number, async v: number, abstract u: number, accessor s: number) {}
}
interface I3 {
    readonly m(): void;
    accessor a: number;
    readonly readonly r: number;
    public public pp: number;
}
interface I4 {
    static [k: string]: number;
}
function blk() {
    static function s1() {}
    async class C1 {}
    abstract class A1 {}
    declare var d1: number;
    abstract function f1(): void;
    async function ok1() {}
    async static function s2() {}
}
namespace NS {
    private function f() {}
    abstract class A {}
}
abstract class D {
    abstract async m3(): Promise<void>;
    static abstract m1(): void;
    abstract #p: number;
    accessor abstract b: number;
}
class E {
    abstract readonly m5(): void;
    abstract constructor() {}
}
class F {
    async static {}
}
let o2 = { async async m() {}, public get g() { return 1; }, static m2() {} };
type T21<in out in T> = { x: T };
type T23<out in T> = { x: T };
