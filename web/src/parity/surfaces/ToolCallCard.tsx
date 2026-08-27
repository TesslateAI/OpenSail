/**
 * ToolCallCard + FileChip — the ChatHost surfaces for tool executions.
 * Source: mock surfaces.css § .tool-card/.file-chip (thread-safe, no shrink).
 */
import type { ReactNode } from "react";
import { cx } from "../../design-system/cx";
import type { FileChipModel, ToolCallModel } from "../presentation/models";

export interface ToolCallCardProps {
  tool: ToolCallModel;
  /** Rendered next to the status name (chevron, spinner, etc.). */
  trailing?: ReactNode;
  collapsed?: boolean;
  onToggle?: () => void;
}

export function ToolCallCard({ tool, trailing, collapsed, onToggle }: ToolCallCardProps): ReactNode {
  return (
    <div className={cx("kds-tool-card", `kds-${tool.status}`)}>
      <button
        type="button"
        className="kds-tool-head"
        onClick={onToggle}
        aria-expanded={collapsed === undefined ? undefined : !collapsed}
      >
        <span className="kds-tool-name">{tool.name}</span>
        {trailing ?? null}
      </button>
      {!collapsed ? (
        <>
          {tool.command !== undefined ? <div className="kds-tool-cmd">{tool.command}</div> : null}
          {tool.output !== undefined ? <div className="kds-tool-out">{tool.output}</div> : null}
        </>
      ) : null}
    </div>
  );
}

export function FileChip({ file }: { file: FileChipModel }): ReactNode {
  return <span className="kds-file-chip">{file.name}</span>;
}
