// Array literal arguments infer from their elements that are not context sensitive first.
declare function compact<T>(array: T[]): T[];
interface Fooer { foo: (v: string) => string; }
declare function makeFooer(): Fooer;
const a = compact([makeFooer(), { foo: (v) => v }]);
const b = compact([{ foo: (v) => v }, makeFooer()]);
declare function pair<T>(x: T, y: T): T;
const c = pair(makeFooer(), { foo: (v) => v });
declare function one<T>(x: { a: T, b: T }): T;
const d = one({ a: makeFooer(), b: { foo: (v) => v } });
export {};
