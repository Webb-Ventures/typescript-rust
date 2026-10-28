// Ecosystem check repro (was ecosystem/repros/check/extra-ts2300-duplicate-identifier.d.ts): extra TS2300: Duplicate identifier '…'.
// @filename: index.d.ts
import 'lib';
declare module 'lib' {
  interface OutputAsset {
    viteMetadata?: string;
  }
}
export {};
// @filename: node_modules/lib/package.json
{ "name": "lib", "version": "1.0.0", "types": "./index.d.mts" }
// @filename: node_modules/lib/index.d.mts
import { h as OutputAsset, v as emit } from './shared.mjs';
export { type OutputAsset, emit };
// @filename: node_modules/lib/shared.d.mts
interface OutputAsset {
  fileName: string;
}
declare function emit(asset: OutputAsset): void;
export { OutputAsset as h, emit as v };
