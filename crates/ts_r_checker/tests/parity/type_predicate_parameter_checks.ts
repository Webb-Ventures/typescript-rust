// A type predicate's parameter must exist (TS1225; TS1230 for a binding-pattern element),
// not be a rest parameter (TS1229), and its type must be assignable to the parameter's
// (TS2677, at the type, in functions, methods, call signatures and function types).
class A { a = 1 }
class C extends A { c = 1 }
declare function f<T>(predicate: (x: {}) => x is T): T;
type P<T> = (x: {}) => x is T;
interface I<T> { m(x: {}): x is T; (y: string): y is T; }
function g(x: string | null): x is string | null | undefined { return true; }
const o = { m(x: number): x is 1 { return true; } };
function b4(...a: any[]): a is A { return true; }
function b5({ a, b, p1 }: any, p2: any): p1 is A { return true; }
function b7({ c: { p1 } }: any, p2: any): p1 is A { return true; }
function missing(y: unknown): x is A { return true; }
function ok(x: A): x is C { return true; }
function thisOk(this: A): this is C { return true; }
