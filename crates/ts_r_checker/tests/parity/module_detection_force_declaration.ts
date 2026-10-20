// `moduleDetection: force` leaves global declaration files as scripts, so their
// `declare module "node:fs"` declarations stay ambient (@types/node).
import { readFileSync } from 'node:fs'
readFileSync(1)
// @filename: node.d.ts
declare module "fs" {
  export function readFileSync(p: string): string;
}
declare module "node:fs" {
  export * from "fs";
}
