/** Props for the browser title projection. */
export interface DocumentTitleProps {
    /** Durable title of the selected session, or undefined for the product title. */
    title?: string;
}
/**
 * Project the selected durable session title into the browser title and
 * restore the build-selected product title when unmounted.
 * @param props - Selected session title projection.
 * @returns No rendered content.
 */
export declare function DocumentTitle({ title }: DocumentTitleProps): null;
//# sourceMappingURL=DocumentTitle.d.ts.map