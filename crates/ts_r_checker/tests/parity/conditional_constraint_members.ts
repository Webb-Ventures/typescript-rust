// date-fns `buildLocalizeFn` and trpc `stream/sse.ts`: the members of a deferred conditional
// type are those of its constraint (the conditional over the check type's constraint when it
// distributes, else the union of its branches), also inside an intersection.
type Args<A extends ((v: number) => number) | undefined> = { values: string[] } & (A extends undefined
  ? { cb?: undefined }
  : { cb: (v: number) => number });

export function build<A extends ((v: number) => number) | undefined>(args: Args<A>) {
  const r = args.cb ? args.cb(1) : 0;
  return r;
}

type C<T> = T extends string ? { a: string } : { a: number; b: 1 };
export function g<T>(c: C<T>) {
  const x: string | number = c.a;
  return c.b;
}

interface Src {
  close(): void;
}
type Cfg = { EventSource: new (url: string) => Src };
export function h<TConfig extends Cfg>(c: InstanceType<TConfig["EventSource"]>) {
  c.close();
  c.open();
}
type Rec<T> = T extends [infer H, ...infer R] ? H | Rec<R> : never;
export function k<T extends unknown[]>(r: Rec<T>) {
  return r;
}
