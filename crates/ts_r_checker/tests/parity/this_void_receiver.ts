// A declared `this: void` allows any receiver, while arguments are still checked.
declare function f(this: void, x: number): void;
const o = { f };
f(1);
o.f(1);
o["f"](1);
(o.f)(1);
o.f!(1);
o.f("bad");

declare function g(this: void, x: number): void;
declare function g(this: void, x: string): void;
const overloaded = { g };
overloaded.g(1);
overloaded.g("ok");

type NoReceiver = void;
declare const aliased: { f(this: NoReceiver): void };
aliased.f();
