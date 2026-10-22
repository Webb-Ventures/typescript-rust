// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ES2023", "lib": ["ES2023"], "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true, "jsx": "preserve"}, "files": ["a.tsx"]}
// @filename: a.tsx
declare global {
  namespace JSX {
    interface Element { kind: "element" }
    interface ElementClass { render(): Element | null }
    interface ElementAttributesProperty { props: {} }
    interface ElementChildrenAttribute { children: {} }
    interface IntrinsicAttributes { key?: string | number }
    interface IntrinsicElements { div: { children?: unknown } }
  }
}

interface ClassComp<P> { new (props: P): { props: P; render(): JSX.Element | null } }
interface FnComp<P> { (props: P): JSX.Element | null }
type AnyComp<P> = ClassComp<P> | FnComp<P>;

// A union tag combines its members' signatures: props are the first parameter.
declare const Elem: AnyComp<{ someKey: string }>;
const bad = <Elem />;
const good = <Elem someKey="ok" />;
const wrong = <Elem someKey={1} />;

// A member without signatures: TS2604.
declare const NotOne: ClassComp<{}> | number;
const notOne = <NotOne />;

// The combined return is checked against `JSX.Element | null | JSX.ElementClass`.
declare const Bad: FnComp<{}> | (() => number);
const badReturn = <Bad />;
export { bad, good, wrong, notOne, badReturn };
