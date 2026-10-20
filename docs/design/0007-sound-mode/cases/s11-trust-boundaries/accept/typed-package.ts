// A trusted package's .d.ts types are used as written.
import { getItem, type Item } from "typed-pkg";

const item: Item = getItem("a");
export const price: number = item.price;
