// Cause:    extra TS2305: Module '…' has no exported member '…'.
// Found in: postcss@8.5 lib/*.d.ts (ContainerProps, NodeProps), ajv (fast-uri's
//           URIComponent), pretty-format (@sinclair/typebox via @jest/schemas)
// Owner:    ts_r_checker
// Expected: tsc 7 (esModuleInterop) reports nothing: `export = Container` where Container is
//           a class merged with a namespace exposes the namespace's members as named
//           exports, so `import Container, { ContainerProps } from './container.js'` works.
// Actual:   ts-r-check reports TS2305 "Module '"./container.js"' has no exported member
//           'ContainerProps'."
// @filename: index.d.ts
import Container, { ContainerProps } from './container.js';
export declare const props: ContainerProps;
export { Container };
// @filename: container.d.ts
declare namespace Container {
  export interface ContainerProps {
    nodes?: string[];
  }
}
declare class Container {
  first: string;
}
export = Container;
