// Destructuring a name that only a template literal index signature covers is no TS2339.
interface I {
  [key: `data-${string}`]: number;
}
declare const obj: I;
const { "data-x": value } = obj;
const { "aria-x": other } = obj;
export { value, other };
