// An explicit any with a stated reason is accepted and listed in the coverage report.
// @ts-r-unsafe S-01: the plugin host passes untyped config objects and validates them itself.
function forward(config: any): unknown {
  return config;
}
export const forwarded: unknown = forward({});
