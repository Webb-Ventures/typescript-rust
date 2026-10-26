/** @import { Shape } from "./types" */
import type { Shape } from "./types";
export type ButtonProps = {
    /**
     * - The button label
     */
    label: string;
    /**
     * Test automation attribute
     */
    "data-test-name"?: string | undefined;
    /**
     * Size in rem,
     * on two lines
     */
    size?: number;
    plain: boolean;
};
export type Count = number;
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
export declare function render(props: ButtonProps): Count;
export type Shapes = Shape[];
export declare class Panel {
    /** @typedef {Shape[]} Shapes */
    /** @type {Shapes} */
    shapes: Shapes;
}
export type Box<T> = {
    value: T;
};
export type Handler = (props_like?: ButtonProps) => void;
export type Trailing = string;
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
