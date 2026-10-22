// Under exactOptionalPropertyTypes an optional property's context has no `missing`, so
// `T` is `number`; a property an object literal gains from its siblings is `missing`, which
// `"a" in z` removes.
declare function match<T>(cb: (value: T) => boolean): T;
declare function foo(pos: { x?: number; y?: number }): boolean;
foo({ y: match(y => y > 0) });
export function main(a: string[] | undefined) {
  const z = a ? { a } : { b: ["there"] };
  "a" in z ? z.a.toString() : z.b.toString();
}
// @filename: tsconfig.json
{
    "compilerOptions": {
        "strict": true, "exactOptionalPropertyTypes": true, "target": "ES2023", "lib": ["ES2023"],
        "module": "ESNext", "moduleResolution": "Bundler", "moduleDetection": "force", "noEmit": true
    }
}
