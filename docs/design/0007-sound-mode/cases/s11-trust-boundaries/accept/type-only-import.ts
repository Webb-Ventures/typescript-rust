// Importing types from a trusted package.
import type { Item } from "typed-pkg";

function total(items: readonly Item[]): number {
  let t = 0;
  for (const i of items) t += i.price;
  return t;
}
export const t: number = total([{ id: "a", price: 2 }]);
