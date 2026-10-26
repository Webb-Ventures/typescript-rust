// A property name reads index signatures keyed by a template literal or string mapping it
// fits (`t.a` for `[k: Lowercase<string>]`), by `.` and by a literal element access. A
// union only has the signatures whose key every member declares.
type A = {
  [name in Lowercase<string>]?: {
    [name in Lowercase<string>]: { a: 1 };
  };
};
declare const a: A;
const a1: never = a.a;
const a2: never = a.a?.a.a;
interface T {
  [name: Lowercase<string>]: { value: "value" };
  [name: Uppercase<string>]: null | { VALUE: "VALUE" };
}
declare const t: T;
const t1: never = t.a;
const t2: never = t.A;
const t3: never = t["a"];
const t4 = t["aB"];
declare const u: T | { [k: string]: number };
const u1 = u.a;
