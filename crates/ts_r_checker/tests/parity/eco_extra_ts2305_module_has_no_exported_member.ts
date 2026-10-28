// Ecosystem check repro (was ecosystem/repros/check/extra-ts2305-module-has-no-exported-member.d.ts): extra TS2305: Module '…' has no exported member '…'.
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
