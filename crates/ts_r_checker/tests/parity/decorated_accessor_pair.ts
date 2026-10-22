// Experimental decorators on both accessors of a get/set pair (TS1207).
// @filename: a.ts
declare function d(...a: any[]): any;
class E {
    @d get x() { return 0; }
    @d @d set x(value: number) { }
    @d static get y() { return 0; }
    @d set y(value: number) { }
    @d get z() { return 0; }
    @d get z() { return 0; }
}
// @filename: tsconfig.json
{"compilerOptions": {"strict": true, "target": "ESNext", "experimentalDecorators": true, "noEmit": true}, "files": ["a.ts"]}
