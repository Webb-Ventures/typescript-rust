// Message event data parsed before use.
import { parse } from "@ts-r.dev/guard";

type ServerEvent = { readonly kind: "tick"; readonly at: number } | { readonly kind: "stop" };
window.addEventListener("message", (e) => {
  const ev = parse<ServerEvent>(e.data);
  console.log(ev.kind);
});
export {};
