/**
 * ReviewThread — the review loop rendered as a conversation (comments look
 * like messages; the resubmit path sits directly under them).
 * Sources:
 *   - mock review.css § .review-*
 *   - mock review-ui.js § ReviewPanel structure
 */
import type { ReactNode } from "react";
import { Badge } from "../../design-system/components/Badge";
import { cx } from "../../design-system/cx";
import type { ReviewCommentModel } from "../presentation/models";
import { toneForReviewState } from "../presentation/models";
import type { ReviewState } from "../presentation/models";

export interface ReviewThreadProps {
  comments: ReadonlyArray<ReviewCommentModel>;
  /** Action row under the thread, e.g. reply input + submit / resubmit. */
  children?: ReactNode;
}

export function ReviewThread({ comments, children }: ReviewThreadProps): ReactNode {
  return (
    <div className="kds-review-thread">
      {comments.map((c) => (
        <div
          key={c.id}
          className={cx(
            "kds-review-comment",
            c.reply && "kds-reply",
            c.resolved && "kds-resolved",
          )}
        >
          <div className="kds-review-comment-body">
            <div className="kds-review-comment-head">
              <span className="kds-review-comment-name">{c.authorName}</span>
              {c.role !== undefined ? (
                <span className="kds-fan-agent-role">{c.role}</span>
              ) : null}
              <span className="kds-faint">{c.at}</span>
            </div>
            <div className="kds-review-comment-text">{c.text}</div>
          </div>
        </div>
      ))}
      {children ?? null}
    </div>
  );
}

/** Header summary line for a review panel: state chip + reviewer. */
export interface ReviewSummaryProps {
  state: ReviewState;
  reviewerName?: string;
  submittedAtLabel?: string;
}

export function ReviewSummary({ state, reviewerName, submittedAtLabel }: ReviewSummaryProps): ReactNode {
  if (state === "none") return null;
  return (
    <div className="kds-review-summary">
      <Badge tone={toneForReviewState(state)}>{state.replace(/_/g, " ")}</Badge>
      {reviewerName !== undefined ? (
        <span className="kds-muted" style={{ marginLeft: 8, fontSize: 12 }}>
          {reviewerName}
          {submittedAtLabel !== undefined ? ` · ${submittedAtLabel}` : ""}
        </span>
      ) : null}
    </div>
  );
}
