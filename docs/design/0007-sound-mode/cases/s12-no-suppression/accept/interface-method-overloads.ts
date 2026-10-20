// Method signature overloads in an interface, implemented by one generic method.
interface Codec {
  encode(x: string): string;
  encode(x: number): number;
}
class Identity implements Codec {
  encode<T extends string | number>(x: T): T {
    return x;
  }
}
const c: Codec = new Identity();
export const e: number = c.encode(1);
