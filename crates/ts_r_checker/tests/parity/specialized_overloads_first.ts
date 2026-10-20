// trpc `server.on("connection", (conn) => ...)` (@types/node): overloads whose parameter is
// annotated with a literal type are tried before the general ones, wherever declared.
interface Server {
  on(event: string, listener: (...args: any[]) => void): this;
  on(event: "close", listener: () => void): this;
  on(event: "connection", listener: (socket: { id: number }) => void): this;
}
declare const s: Server;
s.on("connection", (conn) => {
  const q: null = conn;
});
s.on("other", (x) => {
  const q: null = x;
});
declare function f(x: string): string;
declare function f(x: "a"): number;
const r: null = f("a");
export {};
