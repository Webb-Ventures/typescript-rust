// The apparent type of an intersection gives its interface members
// `this` = the whole intersection.
interface MI<T> { again(): this; v: T }
type M<T> = MI<T> & (() => T);
declare const m: M<number>;
const r: () => number = m.again();
const r2: () => number = m.again().again();
declare const i: MI<string> & { extra: 1 };
const e: 1 = i.again().extra;
