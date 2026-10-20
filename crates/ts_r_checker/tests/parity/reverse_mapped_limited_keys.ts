// Reverse mapping through `keyof T & L` keeps the keys in `L`; literals stay where `T`'s
// constraint takes them; such mapped types are not a `T`.
export {};
type XNumber_ = { x: number }
declare function foo_<T extends XNumber_>(props: {[K in keyof T & keyof XNumber_]: T[K]}): T;
const r = foo_({x: 1, y: 'foo'});
const n: string = r;
declare function bar<T>(props: {[K in keyof T & 'x']: T[K]}): T;
const r2 = bar({x: 1, y: 'foo'});
const n2: string = r2;
declare function baz<T>(props: {[K in keyof T]: T[K]}): T;
const r3 = baz({x: 1, y: 'foo'});
const n3: string = r3;
const checkType_ = <T>() => <U extends T>(value: { [K in keyof U & keyof T]: U[K] }) => value;
const checked_ = checkType_<{x: number, y: string}>()({
  x: 1 as number,
  y: "y",
  z: "z",
});
declare function baz2<T extends { x: number }>(props: {[K in keyof T]: T[K]}): T;
const q1 = baz2({x: 1, y: 'foo'});
const m1: boolean = q1;
declare function bar2<T extends { y: string }>(props: {[K in keyof T & ('x' | 'y')]: T[K]}): T;
const q2 = bar2({x: 1, y: 'foo', z: 2});
const m2: boolean = q2;
declare function bar4<T>(props: {[K in keyof T & ('x' | 'y')]: { v: T[K] }}): T;
const q4 = bar4({x: { v: 1 }, z: { v: 2 }});
const m4: boolean = q4;
type StateConfig<TAction extends string> = {
  entry?: TAction
  states?: Record<string, StateConfig<TAction>>;
};
declare function createMachine<
  TConfig extends StateConfig<TAction>,
  TAction extends string = TConfig["entry"] extends string ? TConfig["entry"] : string,
>(config: { [K in keyof TConfig & keyof StateConfig<any>]: TConfig[K] }): [TAction, TConfig];
createMachine({ entry: "foo", states: { a: { entry: "bar" } }, extra: 12 });
createMachine({ entry: "foo", states: { a: { entry: "foo" } }, extra: 12 });
interface Stuff {
    field: number;
    anotherField: string;
}
function f1<T extends Stuff>(s: { [K in keyof T & keyof Stuff]: T[K] }): T {
    return s;
}
function f2<T extends Stuff>(s: { [K in keyof T]: T[K] }): T {
    return s;
}
function f3<T>(s: { [K in keyof T & 'a']: T[K] }): T {
    return s;
}
