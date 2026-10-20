// Function values in JSX attributes whose props are not modelled (a `JSX` namespace,
// generic components) are not reported as implicitly `any`, also inside conditionals,
// `??`, array and object literals, and in calls on such a parameter (playwright's
// labels.tsx, workbench.tsx, uiModeTestListView.tsx).
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element {}
    interface IntrinsicElements { span: { onClick?: (e: number) => void } }
  }
}
declare const label: string;
declare const flag: boolean;
const a = <span onClick={label ? e => console.log(e) : undefined} />;
const b = <span onClick={flag ? undefined : (e => console.log(e))} />;
declare const h: ((e: number) => void) | undefined;
const c = <span onClick={h ?? (e => console.log(e))} />;
declare function List(p: { render: (item: { tags: string[] }) => string }): JSX.Element;
const d = <List render={item => item.tags.map(tag => tag.trim()).join()} />;
declare function Settings<V extends string>(p: { settings: { value: V; set: (v: V) => void }[] }): JSX.Element;
const s = <Settings settings={[{ value: 'a', set: v => console.log(v) }]} />;
const o = { set: v => v };
export { a, b, c, d, s, o };
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023", "DOM"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
