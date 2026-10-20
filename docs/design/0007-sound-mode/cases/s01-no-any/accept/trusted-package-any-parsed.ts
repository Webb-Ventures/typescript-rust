// typed-pkg declares legacyLookup(): any. Sound mode sees unknown, and parse<T> gives it a type.
import { parse } from "@ts-r.dev/guard";
import { legacyLookup, type Item } from "typed-pkg";

const item: Item = parse<Item>(legacyLookup("a1"));
export const price: number = item.price;
