import { formatElapsedSeconds, type TrajectoryCellProps } from './trajectory-record.ts';
export { formatElapsedSeconds };
export type { AssistantMetricDetail, TrajectoryCellKind, TrajectoryCellProps, } from './trajectory-record.ts';
/**
 * Render one trajectory step cell.
 * @param props - index, kind, text, time, and optional Message metrics.
 * @returns the cell element.
 */
export declare function TrajectoryCell({ index, kind, text, inputDetail: _inputDetail, promptDetail: _promptDetail, previousPromptDetail: _previousPromptDetail, outputDetail: _outputDetail, thinkingDetail: _thinkingDetail, sourceBlocks: _sourceBlocks, outputBlocks: _outputBlocks, schemaDetail: _schemaDetail, assistantMetrics: _assistantMetrics, result: _result, callId: _callId, isError: _isError, timeSeconds, startedAt: _startedAt, input, output, think, selected, className, ...rest }: TrajectoryCellProps): import("react").JSX.Element;
//# sourceMappingURL=TrajectoryCell.d.ts.map