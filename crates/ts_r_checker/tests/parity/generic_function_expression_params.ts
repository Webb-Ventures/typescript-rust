var obj: { f<T>(x: T): T } = { f: <S>(x) => x };
var obj2: <T>(x: T) => T = <S>(x) => x;

class C<T> {
    obj!: <T>(x: T) => T;
}
var c = new C();
c.obj = <S>(x) => x;

declare function f(cb: <T>(x: T) => void): void;
f(<S>(y) => {});
f((z) => {});
const r = (<S>(w) => w)(1);
const n: number = obj2;
