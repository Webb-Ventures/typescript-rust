import { TIMEOUT_MS, type Options } from './lib'
import {
  type Logger,
  facets,
  MASK,
  type Options as O2,
} from './lib'
import { PLAIN, } from './lib'

export {
  TIMEOUT_MS,
  type Options,
  type Logger,
} from './lib'
export { MASK as Mask, type Options as O2 } from './lib'

declare const env: Record<string, string | undefined>
declare function save(): Promise<void>

export const missing = (
  [
    'API_URL',
    'CDN_URL',
  ] as const
).filter((key) => !env[key])

export const rows = [1, 2, 3]
  .filter((n) => n > 1)
  // Largest first.
  .sort((a, b) => b - a)

export const all = [
  ...facets('size', ['s', 'm']),
  // Colours come from the theme.
  ...facets('color', ['red']),
]

export const wait = TIMEOUT_MS * 1_000 + MASK + PLAIN

export function Form({ busy }: { busy: boolean }) {
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        void save()
      }}
    >
      {/* A wrapper, because a button's own
          inline-flex would beat a `hidden`. */}
      <p>
        {busy
          ? 'Saving'
          : 'Ready'}
      </p>
      <div style={{
        padding: '1rem',
      }} />
    </form>
  )
}
