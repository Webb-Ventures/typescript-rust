// Stand-ins for packages in node_modules. The harness treats each ambient module here as a package
// of that name, with the mode from sound.toml or detected (a shorthand declaration means loose).
declare module "typed-pkg" {
  export interface Item {
    readonly id: string;
    readonly price: number;
  }
  export function getItem(id: string): Item;
  export function legacyLookup(id: string): any;
  export const meta: { readonly version: string; readonly extra: any };
}

declare module "loose-pkg";

declare module "verify-pkg" {
  export function score(input: string): number;
}
