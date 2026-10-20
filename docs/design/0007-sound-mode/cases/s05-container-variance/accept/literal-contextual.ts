// An array literal of mixed subtypes typed as Animal[].
import { makeCat, makeDog, type Animal } from "../../_shared/models";

const zoo: Animal[] = [makeDog("a"), makeCat("b"), { name: "c" }];
zoo.push({ name: "d" });
export const size: number = zoo.length;
