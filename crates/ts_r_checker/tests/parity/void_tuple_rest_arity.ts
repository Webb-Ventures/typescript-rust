// Trailing required positions of a rest tuple whose type accepts `void` are optional, as
// for plain parameters (tRPC next's action hooks: `mutate(...args: MutationArgs<TDef>)`
// with `[input: FormData | void | Date | undefined, opts?: O]`).
declare const h: (...args: [input: void | Date, opts?: number]) => void;
h();
declare const h2: (...args: [a: string, input: void | Date]) => void;
h2();
h2('');
declare const f: ((...args: [input: void | Date, opts?: number]) => void) | ((...args: [input: void | Date, opts?: number]) => void);
f();
declare const k: (x: void, ...args: [input: void | Date]) => void;
k();
declare const m: (...args: [input: void | Date, more: string]) => void;
m();
