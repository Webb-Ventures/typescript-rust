// An imported type alias used as a namespace (`CM.Editor`) falls back to a global
// namespace of that name (a UMD `export as namespace`; playwright's codeMirrorWrapper);
// without one it is TS2702.
import type { CM } from './m';
import type { Other } from './m';
let e: CM.Editor | undefined;
let f: CM = null!;
let g: Other.Editor | undefined;
export { e, f, g };
// @filename: m.ts
import type cmType from 'cm';
export type CM = typeof cmType;
export type Other = typeof cmType;
// @filename: node_modules/@types/cm/package.json
{"name": "@types/cm", "version": "1.0.0", "types": "index.d.ts"}
// @filename: node_modules/@types/cm/index.d.ts
export = CM;
export as namespace CM;
declare function CM(): CM.Editor;
declare namespace CM {
  interface Editor { x: number }
}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "types": ["cm"]}, "files": ["a.ts", "m.ts"]}
