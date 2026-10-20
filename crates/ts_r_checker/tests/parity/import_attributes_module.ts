// TS2823 and TS1324: import attributes and import() options need a module that keeps them
// (private-app packages whose tsconfig `extends` is not installed fall back to the
// default module).
// @filename: tsconfig.json
{ "compilerOptions": { "strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ES2022", "moduleResolution": "Bundler", "resolveJsonModule": true, "noEmit": true }, "files": ["a.ts", "b.ts"] }
// @filename: d.json
{ "a": 1 }
// @filename: b.ts
export interface I { a: number }
// @filename: a.ts
import d from './d.json' with { type: 'json' }
import type { I } from './b' with { 'resolution-mode': 'import' }
import type { I as J } from './b' with { type: 'json' }
export {} from './d.json' with { type: 'json' }
export const x: [typeof d, I?, J?] = [d]
export const y = import('./d.json', { with: { type: 'json' } })
