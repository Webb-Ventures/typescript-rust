type Q2<T> = number extends T ? (cb: (n: number) => void) => void : never;
function fn2<T>(arg: Q2<T>) {
  function useT(_arg: T): void {}
  arg(arg => useT(arg));
  arg((n: string) => {});
}
fn2<string | number>(m => m(42));
fn2<number>(m => m(42));

type Q<T> = number extends T ? (n: number) => void : never;
function fn<T>(arg: Q<T>, t: T) {
  arg(10);
  arg(t);
}
