// @target: es2022
// @jsx: preserve
import * as React from "react";
import { Comp, Other } from "./comp";
declare const p: any, name: string, items: any[];
export const a = <div className="x" {...p} key="k">  hello &amp; {"{"} {name}world   <br/></div>;
export const b = <>
    <Comp.Sub a b={1} c="q&quot;" d-e="1" {...p} />
    text on
       two lines
    {/* comment */}
    {...items}
  </>;
export const c = <Other key={name} {...p}><span>only</span></Other>;
export const d = <svg:rect xlink:href="#x" />;
export const e = <div {...p} key="after" />;
export const f = <Comp<string> prop={<i>n</i>} />;
export const g = <div>{items.map(i => <b key={i}>{i}</b>)}</div>;
export const h = <div>
</div>;
