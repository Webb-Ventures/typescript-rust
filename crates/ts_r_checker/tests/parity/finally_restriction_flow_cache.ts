declare const c: boolean;
function g(x: string | number) {
  let y = 0;
  outer: {
    inner: {
      try {
        if (typeof x === "string") break inner;
        break outer;
      } finally { if (c) y = 1; }
    }
    x.toUpperCase();
  }
  x.toFixed();
}
