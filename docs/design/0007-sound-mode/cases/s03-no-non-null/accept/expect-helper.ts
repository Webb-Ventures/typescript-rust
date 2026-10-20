// expect() checks at runtime and returns the non-null type.
import { expect } from "@ts-r.dev/guard";

const names: readonly string[] = ["a"];
export const first: string = expect(names[0], "names is never empty");
