// Type predicate chains (TS1226 over TS2518/TS1227), `#private` members from different
// classes (TS18015), and `?` parameters of function and arrow expressions printed as
// annotated.
export {};
declare let f1: (x: any) => x is string;
declare let f2: (x: any) => x is number;
f1 = f2;
declare let f3: (y: any) => y is string;
f3 = f2;
declare let f4: (x: any, y: any) => y is string;
f4 = f1;
class L { lead() {} }
class F { follow() {} }
declare let g1: () => this is L;
declare let g2: () => this is F;
g1 = g2;
declare let g3: (x: any) => x is L;
g3 = g2;
interface Box { isL(): this is L; }
declare let bx: Box;
declare let by: { isL(): this is F };
bx = by;

class A { #foo: number = 1; }
interface A2 extends A { }
class B { #foo: number = 1; }
const pb: A2 = new B();
class G<T> { #x!: T; }
class H<T> { #x!: T; }
const pg: G<string> = new H<string>();

var a = function then<U>(onFulfill?: (value: string) => U): Promise<U> { return null! };
var b = function then<U>(onFulFill?: (value: number) => U): Promise<U> { return null! };
a = b;
function d(x?: string, y?: number | undefined) { }
const n2: number = d;
const f = (x?: string) => { };
const n4: number = f;
const h = (x?: string | undefined, y?: any) => { };
const n7: number = h;
class K { m(x?: string) { } }
const n6: number = new K().m;
const o = { m(x?: string) { }, f: function (x?: string) { } };
const n9: number = o.m;
const n10: number = o.f;
