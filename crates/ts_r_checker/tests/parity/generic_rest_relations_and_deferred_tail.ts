type Tail<T extends any[]> = ((...t: T) => void) extends (h: any, ...rest: infer R) => void ? R : never;
type A<T extends any[]> = Tail<T>;
function f<T extends any[]>(x: Tail<T>) { const y: number = x; }
type X = Tail<[1, 2, 3]>;
const z: X = [2, 3];
function g2<T extends any[]>(t: T) {
    function f(cb: (x: number, ...args: T) => void) {}
    f((a, b, ...x) => {});
}
type MyTuple = [{}, ...{}[]];
type GenericFunction<T extends MyTuple> = (...fromArgs: T) => void;
class GenericClass<T extends MyTuple> { from: GenericFunction<T> | undefined; }
declare function createClass<T extends MyTuple>(f: GenericFunction<T>): GenericClass<T>;
function consumeClass(c: GenericClass<[string, boolean]>) { }
consumeClass(createClass(str => console.log(str.length)));
function wrap<Args extends unknown[]>(_: (...args: Args) => void) {}
wrap(({ cancelable } = {}) => {});
