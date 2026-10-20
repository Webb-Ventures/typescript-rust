// Generic code takes a Guard<T> made where the type is concrete.
import { guard, type Guard } from "@ts-r.dev/guard";
import type { Settings } from "../../_shared/models";

function decode<T>(text: string, g: Guard<T>): T {
  return g.parse(JSON.parse(text));
}
const settings: Settings = decode("{}", guard<Settings>());
export const theme: string = settings.theme;
