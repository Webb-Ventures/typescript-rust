// Cause:    extra TS2300: Duplicate identifier '…'.
// Found in: vite@8.3.4 dist/node/index.d.ts:2105 and types/metadata.d.ts:32 (also vitest,
//           @vitejs/plugin-react): `declare module "rolldown" { interface OutputAsset {...} }`,
//           where rolldown's dist/index.d.mts does `import { h as OutputAsset } from
//           "./shared/....mjs"` and `export { type OutputAsset }`
// Owner:    ts_r_checker (binder: module augmentation through a re-exported import alias)
// Expected: tsc 7 merges the augmentation into the original interface (the alias resolves
//           to it). No error.
// Actual:   ts-r-check reports TS2300 "Duplicate identifier 'OutputAsset'." at rolldown's
//           `export { type OutputAsset }`.
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
