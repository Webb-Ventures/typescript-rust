type OptionOne = { kind: "one"; s: string; };
type OptionTwo = { kind: "two"; x: number; y: number; };
type Options = OptionOne | OptionTwo;
type OptionHandlers = {
  [K in Options['kind']]: (option: Options & { kind: K }) => string;
}
declare const optionHandlers: OptionHandlers;
function handleOption<K extends Options['kind']>(option: Options & { kind: K }): string {
  const kind = option.kind;
  const k2: typeof kind = 5;
  const handler = optionHandlers[kind];
  return handler(option);
}

function f<K extends "one" | "two">(d: ("one" & K) | ("two" & K), e: "one" & K) {
  const d1: typeof d = 5;
  const e1: typeof e = 5;
}

type Foo1 = { x: number; y: string; };
function getValueConcrete<K extends keyof Foo1>(o: Partial<Foo1>, k: K): Foo1[K] | undefined {
  return o[k];
}
function getValueBad<K extends keyof Foo1>(o: Partial<Foo1>, k: K): Foo1[K] {
  return o[k];
}

type M0 = { a: 1, b: 2 };
type M1 = { [K in keyof Partial<M0>]: M0[K] };
function foo<K extends keyof M0>(m1: M1[K]) {
  m1.toString();
  m1?.toString();
}
