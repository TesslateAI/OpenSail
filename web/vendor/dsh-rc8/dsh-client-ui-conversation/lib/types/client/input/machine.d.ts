/**
 * InputMachine: the pure per-session input state machine.
 * Events in, effects out; zero React / DOM / cordis / ambient
 * clock. Package-private — the SessionInput shell is the only caller and the
 * sole executor of the returned effects.
 *
 * Draft truth: the draft string holds each reference's complete inline display
 * text; the occurrence table carries identity, range, and the owner's cached projections. Every
 * draft mutation is one transaction — draft edit, occurrence reconciliation,
 * and undo-log push are atomic inside dispatch() — and bumps draftRev, which
 * is what lets span CAS reduce to a revision-equality check: equal rev ⟹
 * identical draft ⟹ identical span content. Callers observe mutation success
 * as a draftRev advance (begin-command / insert-ref / consume-token /
 * paste-upgrade all answer their bail events this way).
 */
import type { ReferenceInsert } from '@deepseek-ai/dsh-client-ui-input-trigger/client';
import type { InputEffect, InputEvent, InputMachineOptions, InputState } from './contract.ts';
/** Legacy fixed-width object replacement character rejected from pasted text. */
export declare const PLACEHOLDER = "\uFFFC";
/**
 * Build the inline draft text whose leading marker is decorated as the
 * reference icon in the backdrop.
 * @param reference - reference insertion with its cached display projection.
 * @returns display text with one marker glyph followed by the complete label.
 */
export declare function referenceDraftText(reference: Pick<ReferenceInsert, 'label'>): string;
/**
 * Expand the draft's reference ranges into their occurrences' clipboard text
 * for persistence and clipboard projection. Table order is offset order, so
 * one linear walk pairs ranges with entries.
 * @param state - published input state.
 * @returns the plain-text projection of the draft.
 */
export declare function projectClipboard(state: Pick<InputState, 'draft' | 'occurrences'>): string;
/**
 * Pure input machine, one instance per session (per-session isolation is by
 * construction). The machine constructs one AbortController per SubmitAttempt
 * at enter time and aborts it itself on release; the shell never aborts, it
 * only observes attempt.signal on its adjudicate/submit promises. Stale
 * attempts (any adjudicated / adjudication-failed / submit-settled whose seq
 * is not the in-flight one) are dropped: same state, zero effects.
 */
export declare class InputMachine {
    private draft;
    private draftRev;
    private phase;
    private claim;
    private occurrences;
    private occurrenceSeq;
    private seq;
    private inflight;
    private log;
    private redoStack;
    /** Open single-char typing run: the next contiguous char within the window coalesces. */
    private typingRun;
    private paste;
    private pasteSeq;
    private readonly mergeWindowMs;
    private readonly now;
    constructor(options?: InputMachineOptions);
    /** Read-only snapshot of the machine state (queue always empty at this tier). */
    get state(): InputState;
    /**
     * Feed one event through the machine.
     * @param ev - Input event; the single write path for all input state.
     * @returns Effects for the shell to execute in order; empty on no-ops, locks, and dropped stale events.
     */
    dispatch(ev: InputEvent): readonly InputEffect[];
    /** Adopt a new draft: bump the revision (the span-CAS invalidation point). */
    private adopt;
    /** Push one undo unit (before-state), trim the ring, and cut the redo chain. */
    private pushTxn;
    /**
     * Reconcile the occurrence table with one edit (old-draft coordinates):
     * entries past the range shift by the length delta; an edit that intersects
     * a reference range removes its structured occurrence and leaves the edited
     * characters as ordinary draft text.
     */
    private reconcile;
    /** Claimed integrity watch: any mutation that breaks the token prefix releases the claim. */
    private watchClaim;
    /** Mint one occurrence at a draft offset. */
    private mint;
    /** Splice minted entries into the offset-sorted table. */
    private withMinted;
    private onDraftChanged;
    /** Span CAS: revision equality (content identity follows) plus bounds sanity. */
    private casOk;
    private onBeginCommand;
    private onInsertRef;
    /**
     * Shared reference-insertion transaction: replace [span) with one inline
     * occurrence (insert-ref and paste-upgrade both land here). A separating
     * space follows the reference unless one is already next.
     * @returns the inserted length (display text plus optional gap).
     */
    private replaceSpanWithChip;
    /**
     * Guarded token deletion after business success (popup settle / menu-pick
     * execute). No effect signals success: the caller reads the draftRev
     * advance off the published state (same currency as the other bail verbs).
     */
    private onConsumeToken;
    /**
     * Owner-resolution style bits: exactly the listed occurrences render
     * invalid. Not a transaction — the draft, revision, and undo log are
     * untouched (invalidation never deletes or rewrites chips).
     */
    private onSetInvalid;
    private onUndo;
    private onRedo;
    /**
     * Paste as one transaction: the text (reference-placeholder-sanitized) replaces the
     * selection; hot-snapshot sync matches componentize inside the SAME
     * transaction (one undo returns to pre-paste); a match attempt opens for
     * the async remainder while the phase still accepts reference mutations.
     */
    private onPasteBegin;
    /**
     * Async match landed: upgrade one pasted token to a chip as an INDEPENDENT
     * transaction (undo #1 → the token text, undo #2 → pre-paste). The attempt
     * stays current — later tokens re-CAS against the advanced draftRev.
     */
    private onPasteUpgrade;
    /** Mint the next SubmitAttempt and take the in-flight slot. */
    private beginAttempt;
    private onEnter;
    private onAdjudicated;
    private onAdjudicationFailed;
    private onSubmitSettled;
    /** Cut undo state after an accepted image-only send. */
    private onSendCommitted;
    private onRelease;
}
//# sourceMappingURL=machine.d.ts.map