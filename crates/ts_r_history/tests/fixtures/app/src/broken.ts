import { area, type Shape } from "./shapes";
import { missing } from "./math";

export function bad(s: Shape): number {
  if (s.kind === "triangle") {
    return 0;
  }
  return area(s) + undefinedName;
}

const settings: Settings = { theme: "blue", fontSize: 1 };
export const x: boolean = settings.fontSize;
