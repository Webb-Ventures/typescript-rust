// Inference into a method signature's parameters is bivariant: a callback's
// `any` parameter is an ordinary candidate (`new Proxy({}, handler)`).
interface CI { add(): void; on?(): void }
function f1(): CI {
  return new Proxy({}, { get: (obj: any, prop: string | symbol) => 1 });
}
function f2(): CI {
  return new Proxy({}, {});
}
declare function id<T extends object>(t: T, h: { get?(t: T): any }): T;
function f3(): CI {
  return id({}, { get: (obj: any) => 1 });
}
function f4(): CI {
  return id({}, {});
}
function f5(): CI {
  return id({ a: 1 }, { get: (obj: any) => 1 });
}
const v: CI = id({}, { get: (obj: any) => 1 });
