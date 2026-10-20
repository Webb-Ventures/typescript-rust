import { describe } from "./augment";
import { Canvas, totalArea, type Shape } from "./shapes";
import { banner, counter, labels, pages } from "./util";
import { pick } from "./math";

const shapes: Shape[] = [
  { kind: "circle", center: { x: 0, y: 0 }, radius: 2 },
  { kind: "rect", box: { min: { x: 0, y: 0 }, max: { x: 1, y: 1 }, label: "unit" } },
];

const canvas = new Canvas().add(shapes[0]).add(shapes[1]);
const n: number = canvas.count;
const total: string = totalArea(shapes);
const name = describe({ min: { x: 1, y: 2 }, max: { x: 3, y: 4 } });
const p = pick({ a: 1, b: "x", c: true }, ["a", "c"]);
const wrong: number = p.b;
const pg = pages([1, 2, 3]);
const l = labels([1, 2]).lastItem();
counter.next(1);
console.log(banner, n, total, name, pg, l, wrong);
