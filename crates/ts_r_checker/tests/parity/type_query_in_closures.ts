function test1(a: number | string) {
  if (typeof a === "number") {
    const fn = (arg: typeof a) => true;
    const g = function (arg: typeof a) { return true; };
    let v: typeof a = "";
    const h = () => { let w: typeof a = ""; };
    return fn;
  }
  return;
}
test1(0)?.("");
export {};
