// @ts-r off
// A non-sound local file. Sound files see its exports like a trusted package: any becomes unknown.
export const legacyConfig: any = { theme: "dark", fontSize: 14 };
export function legacyCount(input: any): any {
  return input.length;
}
