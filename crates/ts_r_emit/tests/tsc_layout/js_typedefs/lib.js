/** @import { Shape } from "./types" */

/**
 * Options for a button.
 * @typedef {Object} ButtonProps
 * @property {string} label - The button label
 * @property {string | undefined} [data-test-name] Test automation attribute
 * @property {number} [size=1]
 *   Size in rem,
 *   on two lines
 * @property {boolean} plain
 */
/** @typedef {number} Count */

/**
 * @param {ButtonProps} props
 * @returns {Count}
 */
export function render(props) {
    return props.label.length;
}

export class Panel {
    /** @typedef {Shape[]} Shapes */
    /** @type {Shapes} */
    shapes = [];
}

/**
 * @template T
 * @typedef {{ value: T }} Box
 */

/**
 * @callback Handler
 * @param {ButtonProps} [props-like]
 * @returns {void}
 */

/** @typedef {string} Trailing */
