// The rest of a generic object is `Omit<T, K>`; a class instance also omits its methods and
// accessors. Its members are the lower bound of the key type `Exclude<keyof T, K>`.
class A {
  constructor(public publicProp: string, private privateProp: string) {}
  method() {
    const { ...rest1 } = this;
    rest1.publicProp;
    rest1.method;
    rest1.privateProp;
  }
}
function f<T extends { a: string; b: number }>(t: T, p: Pick<T, Exclude<keyof T, "a">>) {
  const { a, ...r } = t;
  const n: number = r;
  r.a;
  r.b;
  p.zzz;
  p.b;
}
