// Destructuring an external value reads properties off it.
const { theme } = JSON.parse('{"theme":"dark"}'); // @expect TSR1401
export const t: unknown = theme;
