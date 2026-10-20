import { TIMEOUT_MS } from './lib';
import { facets, MASK, } from './lib';
import { PLAIN, } from './lib';
export { TIMEOUT_MS, } from './lib';
export { MASK as Mask } from './lib';
export const missing = [
    'API_URL',
    'CDN_URL',
].filter((key) => !env[key]);
export const rows = [1, 2, 3]
    .filter((n) => n > 1)
    // Largest first.
    .sort((a, b) => b - a);
export const all = [
    ...facets('size', ['s', 'm']),
    // Colours come from the theme.
    ...facets('color', ['red']),
];
export const wait = TIMEOUT_MS * 1000 + MASK + PLAIN;
export function Form({ busy }) {
    return (<form onSubmit={(e) => {
            e.preventDefault();
            void save();
        }}>
      {/* A wrapper, because a button's own
            inline-flex would beat a `hidden`. */}
      <p>
        {busy
            ? 'Saving'
            : 'Ready'}
      </p>
      <div style={{
            padding: '1rem',
        }}/>
    </form>);
}
