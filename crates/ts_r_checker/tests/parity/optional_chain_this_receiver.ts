// Only the chain's short-circuit undefined is omitted from a call's receiver.
interface I { m(this: I): void }
declare const o: { inner: I } | undefined;
o?.inner.m();
o?.inner["m"]();
o?.inner.m?.();

declare const make: (() => I) | undefined;
make?.().m();

declare const maybe: { inner: I | undefined } | undefined;
maybe?.inner?.m();

declare const nullable: { inner: I | null } | undefined;
nullable?.inner?.m();
