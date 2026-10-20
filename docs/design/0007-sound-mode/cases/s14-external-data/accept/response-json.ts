// Response.json() parsed before it is returned as Order.
import { parse } from "@ts-r.dev/guard";

interface Order {
  readonly id: string;
  readonly total: number;
}
async function loadOrder(res: Response): Promise<Order> {
  return parse<Order>(await res.json());
}
const order: Order = await loadOrder(new Response('{"id":"a","total":1}'));
export const total: number = order.total;
