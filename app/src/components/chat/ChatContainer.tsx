import { useState, useRef, useEffect, useCallback, useMemo, type RefObject } from 'react';
import { useNavigate } from 'react-router-dom';
import { useHotkeys } from 'react-hotkeys-hook';
import { Loader2, FileCode, X, List, Plus, Plug, Pause } from 'lucide-react';
import { PencilSimple, Storefront } from '@phosphor-icons/react';
import { nodeConfigEvents } from '../../utils/nodeConfigEvents';
import { notifyFileMutationsFromAgentEvent } from '../../utils/agentFileEvents';
import { useCommandHandlers } from '../../contexts/CommandContext';
import { ChatMessage } from './ChatMessage';
import { ChatInput } from './ChatInput';
import { type EditMode } from './EditModeStatus';
import { ApprovalRequestCard } from './ApprovalRequestCard';
import { WorkspaceAttachCard } from './WorkspaceAttachCard';
import { ChatSessionPopover } from './ChatSessionPopover';
import { ToolDebugModal } from './ToolDebugModal';
import { createWebSocket, chatApi, marketplaceApi } from '../../lib/api';
import { useAuth } from '../../contexts/AuthContext';
import { useAgentRuns } from '../../contexts/useAgentRuns';
import toast from 'react-hot-toast';
import AgentMessage from '../AgentMessage';
import {
  type AgentMessageData,
  type DBMessage,
  type SerializedAttachment,
  type ChatMention,
} from '../../types/agent';
import { type ChatAgent } from '../../types/chat';

function formatAgentError(raw: string): string {
  if (raw.includes('does not exist') || raw.includes('NotFoundError'))
    return 'Model not available. Try selecting a different model.';
  if (raw.includes('429') || raw.includes('rate limit'))
    return 'Rate limited. Please wait a moment and try again.';
  if (raw.includes('timeout') || raw.includes('timed out'))
    return 'Request timed out. Please try again.';
  if (raw.includes('401') || raw.includes('authentication') || raw.includes('api_key'))
    return 'Authentication error. Check your API key configuration.';
  if (raw.includes('Resource limit')) return 'Resource limit exceeded for this session.';
  if (raw.includes('budget') || raw.includes('Budget'))
    return 'Usage limit reached. Please try again or purchase more credits.';
  return raw.length > 120 ? raw.slice(0, 120) + '...' : raw;
}

interface Message {
  id: string;
  type: 'user' | 'ai' | 'approval_request' | 'workspace_attach_request';
  content: string;
  agentData?: AgentMessageData;
  agentIcon?: string;
  agentAvatarUrl?: string;
  agentType?: string;
  toolCalls?: Array<{
    name: string;
    description: string;
  }>;
  actions?: Array<{
    label: string;
    onClick: () => void;
  }>;
  timestamp?: string;
  // Approval-specific fields
  approvalId?: string;
  toolName?: string;
  toolParameters?: Record<string, unknown>;
  toolDescription?: string;
  attachments?: SerializedAttachment[];
  // Workspace-attach (request_workspace tool) prompt fields.
  workspaceAttachInputId?: string;
  workspaceAttachReason?: string;
  workspaceAttachCandidates?: Array<{
    id: string;
    name: string;
    slug: string;
    compute_tier: string;
    created_via?: string | null;
  }>;
}

interface StreamingFile {
  fileName: string;
  isStreaming: boolean;
}

interface ChatContainerProps {
  projectId: number;
  containerId?: string; // Container ID for container-scoped agents
  viewContext?: 'graph' | 'builder' | 'terminal' | 'kanban'; // UI view context for scoped tools
  agents: ChatAgent[];
  currentAgent: ChatAgent;
  onSelectAgent: (agent: ChatAgent) => void;
  onFileUpdate: (filePath: string, content: string) => void;
  slug?: string;
  projectName?: string;
  className?: string;
  sidebarExpanded?: boolean;
  isDocked?: boolean; // When true, renders as docked panel instead of floating
  isPointerOverPreviewRef?: RefObject<boolean>; // Tracks if mouse is over preview iframe
  prefillMessage?: string | null;
  /** Structured @-mentions to seed alongside ``prefillMessage`` (e.g. from
   *  DataPanel's "Ask agent" button). See ChatInput.prefillMentions. */
  prefillMentions?: ChatMention[] | null;
  onPrefillConsumed?: () => void;
  onExpandedChange?: (expanded: boolean) => void;
  // Lifecycle event callbacks
  onIdleWarning?: (minutesLeft: number) => void;
  onEnvironmentStopping?: () => void;
  onEnvironmentStopped?: (reason: string) => void;
  onVolumeReady?: () => void;
  disabled?: boolean;
  /**
   * Deep-link target session. When set on mount, seeds `currentChatId` so
   * the "load most recent session" effect doesn't override it. When the
   * prop later changes (e.g. user clicks a different chat in the sidebar
   * while the builder is already open), we swap to that session in place.
   */
  initialChatId?: string | null;
  /**
   * Open the persistent Config tab. Wired from ProjectPage so the chat-side
   * pause banner can take the user to the relevant card with one click.
   */
  onOpenConfigTab?: () => void;
}

export function ChatContainer({
  projectId,
  containerId,
  viewContext,
  agents: initialAgents,
  currentAgent: initialCurrentAgent,
  onSelectAgent,
  onFileUpdate,
  slug: projectSlug,
  projectName = 'project',
  className = '',
  sidebarExpanded = true,
  isDocked = false,
  isPointerOverPreviewRef,
  prefillMessage,
  prefillMentions,
  onPrefillConsumed,
  onExpandedChange,
  onIdleWarning,
  onEnvironmentStopping,
  onEnvironmentStopped,
  onVolumeReady,
  disabled: disabledProp,
  initialChatId = null,
  onOpenConfigTab,
}: ChatContainerProps) {
  const navigate = useNavigate();
  const { hasRole } = useAuth();
  const isAdmin = hasRole('admin');
  const [showDebugModal, setShowDebugModal] = useState(false);
  const [isExpanded, setIsExpanded] = useState(false);
  // Pinned-banner pauses: agent has called request_node_config(wait_for_input=True)
  // and is parked on a pending input. Cleared on resume / cancel.
  const [pendingPauses, setPendingPauses] = useState<
    Array<{ inputId: string; containerId: string; containerName: string }>
  >([]);
  useEffect(() => {
    const unsubs: Array<() => void> = [];
    unsubs.push(
      nodeConfigEvents.on('user-input-required', (payload) => {
        setPendingPauses((prev) =>
          prev.some((p) => p.inputId === payload.input_id)
            ? prev
            : [
                ...prev,
                {
                  inputId: payload.input_id,
                  containerId: payload.container_id,
                  containerName: payload.container_name,
                },
              ]
        );
      })
    );
    unsubs.push(
      nodeConfigEvents.on('node-config-resumed', (payload) => {
        setPendingPauses((prev) => prev.filter((p) => p.inputId !== payload.input_id));
      })
    );
    unsubs.push(
      nodeConfigEvents.on('node-config-cancelled', (payload) => {
        setPendingPauses((prev) => prev.filter((p) => p.inputId !== payload.input_id));
      })
    );
    return () => {
      for (const u of unsubs) u();
    };
  }, []);
  useEffect(() => {
    onExpandedChange?.(isExpanded);
  }, [isExpanded, onExpandedChange]);
  const [messages, setMessages] = useState<Message[]>([]);
  const [agents, setAgents] = useState<ChatAgent[]>(initialAgents);
  const [currentAgent, setCurrentAgent] = useState<ChatAgent>(initialCurrentAgent);
  const [toolCallsCollapsed, setToolCallsCollapsed] = useState(true);
  const [availableSkills, setAvailableSkills] = useState<{ name: string; description: string }[]>(
    []
  );
  const [activeMcpServers, setActiveMcpServers] = useState<{ name: string; slug: string }[]>([]);
  const [modelVisionMap, setModelVisionMap] = useState<Record<string, boolean>>({});

  // Fetch model vision support map once
  useEffect(() => {
    let cancelled = false;
    marketplaceApi
      .getAvailableModels()
      .then((data) => {
        if (cancelled) return;
        const map: Record<string, boolean> = {};
        for (const m of data.models || []) {
          map[m.id] = m.supports_vision ?? false;
        }
        setModelVisionMap(map);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  const currentModelId = currentAgent.selectedModel || currentAgent.model;
  const currentModelSupportsVision = currentModelId ? modelVisionMap[currentModelId] : undefined;
  const [editMode, setEditMode] = useState<EditMode>(() => {
    const stored = localStorage.getItem(`editMode:${projectId}`);
    return stored === 'ask' || stored === 'allow' || stored === 'plan' ? stored : 'ask';
  });
  const editModeRef = useRef<EditMode>(editMode);
  useEffect(() => {
    editModeRef.current = editMode;
    localStorage.setItem(`editMode:${projectId}`, editMode);

    // Auto-approve any pending approval cards when switching to "Allow All Edits"
    if (editMode === 'allow') {
      setMessages((prev) => {
        const pending = prev.filter((m) => m.type === 'approval_request' && m.approvalId);
        if (pending.length === 0) return prev;

        for (const msg of pending) {
          const id = msg.approvalId!;
          if (wsRef.current?.readyState === WebSocket.OPEN) {
            wsRef.current.send(
              JSON.stringify({
                type: 'approval_response',
                approval_id: id,
                response: 'allow_all',
              })
            );
          } else {
            chatApi
              .sendApprovalResponse(id, 'allow_all')
              .catch((err) => console.error('[APPROVAL] Auto-approve failed:', err));
          }
        }

        return prev.filter((m) => m.type !== 'approval_request');
      });
    }
  }, [editMode]);
  const agentRuns = useAgentRuns();
  const [isStreaming, setIsStreaming] = useState(false);
  const [agentExecuting, setAgentExecuting] = useState(false);
  const [currentStream, setCurrentStream] = useState('');
  const [streamingFiles, setStreamingFiles] = useState<Map<string, StreamingFile>>(new Map());
  const [isLoadingHistory, setIsLoadingHistory] = useState(false);
  // Use matchMedia for initial value to avoid forced reflow from reading window.innerWidth
  const [isDesktop, setIsDesktop] = useState(() => {
    if (typeof window !== 'undefined' && window.matchMedia) {
      return window.matchMedia('(min-width: 768px)').matches;
    }
    return true; // Default to desktop
  });

  // When docked, always show as expanded
  const effectiveIsExpanded = isDocked || isExpanded;

  // Seed from initialChatId so the "load most recent session" fallback in
  // loadSessions() (gated on `!currentChatId`) leaves the deep-linked
  // session intact on first paint.
  const [currentChatId, setCurrentChatId] = useState<string | null>(initialChatId ?? null);
  const [sessions, setSessions] = useState<
    {
      id: string;
      title: string | null;
      origin: string;
      status: string;
      created_at: string;
      updated_at: string | null;
      message_count: number;
    }[]
  >([]);
  const [showSessionPopover, setShowSessionPopover] = useState(false);
  const [isRenamingTitle, setIsRenamingTitle] = useState(false);
  const [renameTitleValue, setRenameTitleValue] = useState('');
  const [_reconnecting, setReconnecting] = useState(false);
  const [_sessionTransitioning, setSessionTransitioning] = useState(false);

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const messagesContainerRef = useRef<HTMLDivElement>(null);
  const sessionsButtonRef = useRef<HTMLButtonElement>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const isUserScrollingRef = useRef(false);
  const animatedMessagesRef = useRef<Set<string>>(new Set());
  const isMountedRef = useRef(true);
  const agentTaskIdRef = useRef<string | null>(null);
  const currentChatIdRef = useRef<string | null>(currentChatId);
  useEffect(() => {
    currentChatIdRef.current = currentChatId;
  }, [currentChatId]);

  // Sidebar deep-link reactor: when the user clicks a different project
  // chat in the sidebar while this builder is already mounted, the parent
  // re-renders with a new `initialChatId`. Mirror handleSelectSession so
  // the swap looks identical to picking from the in-chat session popover.
  // Deps intentionally exclude currentChatId — including it would let an
  // internal session change (e.g. "New chat") trigger this effect and
  // bounce us back to the stale prop value.
  useEffect(() => {
    if (initialChatId && initialChatId !== currentChatIdRef.current) {
      setSessionTransitioning(true);
      setMessages([]);
      animatedMessagesRef.current.clear();
      setCurrentChatId(initialChatId);
      const t = setTimeout(() => setSessionTransitioning(false), 200);
      return () => clearTimeout(t);
    }
  }, [initialChatId]);

  // Track mounted state to guard orphaned SSE callbacks after unmount
  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  // Load chat history from database — reload when session changes.
  // Awaits loadChatHistory BEFORE checking for an active task so that the
  // history is already rendered and checkActiveTask won't overwrite it.
  useEffect(() => {
    let cancelled = false;
    let activeEventSource: EventSource | null = null;
    let reconnectTimeoutId: ReturnType<typeof setTimeout> | null = null;

    // Reset agent state when project/session changes
    setAgentExecuting(false);
    agentTaskIdRef.current = null;

    const loadChatHistory = async (): Promise<Message[]> => {
      setIsLoadingHistory(true);
      try {
        const dbMessages: DBMessage[] = await chatApi.getSessionMessages(
          projectId.toString(),
          currentChatId || undefined
        );

        const expandedMessages: Message[] = [];

        dbMessages.forEach((msg, idx) => {
          // Map 'assistant' role to 'ai' type for frontend
          const messageType = msg.role === 'assistant' ? 'ai' : 'user';

          // For user messages or non-agent assistant messages, add as-is
          // Skip messages with empty content to prevent empty chat bubbles
          if (messageType === 'user' || !msg.message_metadata?.agent_mode) {
            if (msg.content && msg.content.trim()) {
              expandedMessages.push({
                id: `msg-${idx}`,
                type: messageType,
                content: msg.content,
                timestamp: msg.created_at,
                attachments: msg.message_metadata?.attachments as
                  | SerializedAttachment[]
                  | undefined,
              });
            }
            return;
          }

          // For agent messages, split iterations into separate messages
          // Find agent icon from initialAgents if available
          const agentData =
            initialAgents.length > 0
              ? initialAgents.find((a) => a.name === msg.message_metadata?.agent_type)
              : null;
          const agentIcon = agentData?.icon || '🤖';
          const agentAvatarUrl = agentData?.avatar_url;
          const agentType = msg.message_metadata.agent_type;
          const finalResponse = msg.content && msg.content.trim() ? msg.content : '';

          // Add each step as a separate message (filter out steps with no content)
          if (msg.message_metadata.steps && msg.message_metadata.steps.length > 0) {
            msg.message_metadata.steps.forEach((step, stepIdx) => {
              // Only add steps that have tool calls or thoughts (match AgentMessage filtering)
              const hasContent =
                (step.tool_calls && step.tool_calls.length > 0) ||
                (step.thought && step.thought.trim());
              if (!hasContent) return;

              expandedMessages.push({
                id: `msg-${idx}-step-${stepIdx}`,
                type: 'ai',
                content: '', // Don't include final response in steps
                agentData: {
                  steps: [step],
                  iterations: step.iteration || stepIdx + 1,
                  tool_calls_made: step.tool_calls?.length || 0,
                  completion_reason: 'step_complete',
                },
                agentIcon,
                agentAvatarUrl,
                agentType,
              });
            });

            // Always add final response as a separate message if it exists
            if (finalResponse) {
              expandedMessages.push({
                id: `msg-${idx}-final`,
                type: 'ai',
                content: finalResponse,
                agentData: {
                  steps: [],
                  iterations: 0,
                  tool_calls_made: 0,
                  completion_reason: 'complete',
                },
                agentIcon,
                agentAvatarUrl,
                agentType,
              });
            }
          } else if (finalResponse) {
            // If no steps but has final response, create a message with empty agentData
            expandedMessages.push({
              id: `msg-${idx}-result`,
              type: 'ai',
              content: finalResponse,
              agentData: {
                steps: [],
                iterations: 0,
                tool_calls_made: 0,
                completion_reason: 'complete',
              },
              agentIcon,
              agentAvatarUrl,
              agentType,
            });
          } else if (msg.message_metadata?.completion_reason === 'in_progress') {
            // Skip — checkActiveTask handles live thinking state.
            // Stale in_progress messages from disconnects/crashes
            // should not render thinking dots from history.
          }
        });

        if (!cancelled) {
          setMessages(expandedMessages);
        }
        return expandedMessages;
      } catch (error) {
        console.error('[CHAT] Failed to load chat history:', error);
        if (!cancelled) setMessages([]);
        return [];
      } finally {
        if (!cancelled) setIsLoadingHistory(false);
      }
    };

    // Check for active agent task and reconnect to SSE stream
    const checkActiveTask = async (currentMessages: Message[]) => {
      if (cancelled) return;
      try {
        const activeTask = await chatApi.getActiveTask(
          projectId.toString(),
          currentChatId || undefined
        );
        if (!activeTask || cancelled) return;

        setReconnecting(true);
        setAgentExecuting(true);
        agentTaskIdRef.current = activeTask.task_id;

        const thinkingId = `reconnect-${activeTask.task_id}`;

        // Only add a thinking placeholder if loadChatHistory didn't already
        // produce an in-progress message for this task.
        const alreadyHasPlaceholder = currentMessages.some(
          (m) => m.agentData?.completion_reason === 'in_progress'
        );
        if (!alreadyHasPlaceholder) {
          const thinkingMsg: Message = {
            id: thinkingId,
            type: 'ai',
            content: '',
            agentData: {
              steps: [],
              iterations: 0,
              tool_calls_made: 0,
              completion_reason: 'in_progress',
            },
          };
          setMessages((prev) => [...prev, thinkingMsg]);
        }

        // Subscribe to events with safety timeout
        const eventSource = chatApi.subscribeToTask(activeTask.task_id);
        activeEventSource = eventSource;

        const cleanupReconnect = () => {
          if (reconnectTimeoutId) clearTimeout(reconnectTimeoutId);
          eventSource.close();
          activeEventSource = null;
          if (!cancelled) {
            setAgentExecuting(false);
            setReconnecting(false);
            agentTaskIdRef.current = null;
          }
        };

        // Safety timeout: if no events arrive within 30s, task is likely stale
        const resetSafetyTimeout = () => {
          if (reconnectTimeoutId) clearTimeout(reconnectTimeoutId);
          reconnectTimeoutId = setTimeout(() => {
            cleanupReconnect();
            if (!cancelled) {
              setMessages((prev) =>
                prev.map((m) =>
                  m.id === thinkingId && m.agentData?.completion_reason === 'in_progress'
                    ? { ...m, agentData: { ...m.agentData!, completion_reason: 'error' } }
                    : m
                )
              );
            }
          }, 30000);
        };
        resetSafetyTimeout();

        eventSource.onmessage = (event) => {
          if (cancelled) return;
          try {
            const data = JSON.parse(event.data);
            resetSafetyTimeout(); // Got data, reset the silence timeout
            if (data.type === 'agent_step') {
              // Notify panels when a kanban tool call succeeds
              const toolCalls = data.data?.tool_calls || [];
              const toolResults = data.data?.tool_results || [];
              const hasSuccessfulKanban = toolCalls.some(
                (tc: { name: string }, idx: number) =>
                  tc.name === 'kanban' && toolResults[idx]?.result?.success !== false
              );
              if (hasSuccessfulKanban) {
                window.dispatchEvent(new CustomEvent('kanban-updated'));
              }

              setMessages((prev) => {
                const updated = [...prev];
                const orig = updated[updated.length - 1];
                if (orig && orig.agentData?.completion_reason === 'in_progress') {
                  const steps = orig.agentData.steps || [];
                  const lastMsg = { ...orig };
                  lastMsg.agentData = {
                    ...lastMsg.agentData!,
                    steps: [...steps, data.data],
                  };
                  updated[updated.length - 1] = lastMsg;
                }
                return updated;
              });
            } else if (data.type === 'approval_required') {
              const approvalData = data.data || data;
              if (editModeRef.current === 'allow') {
                chatApi
                  .sendApprovalResponse(approvalData.approval_id, 'allow_all')
                  .catch((err: unknown) => console.error('[APPROVAL] Auto-approve failed:', err));
              } else {
                const approvalMessage: Message = {
                  id: `approval-${Date.now()}`,
                  type: 'approval_request',
                  content: '',
                  approvalId: approvalData.approval_id,
                  toolName: approvalData.tool_name,
                  toolParameters: approvalData.tool_parameters,
                  toolDescription: approvalData.tool_description,
                };
                setMessages((prev) => [...prev, approvalMessage]);
              }
            } else if (data.type === 'complete') {
              const completeData = data.data || {};
              const finalContent = completeData.final_response || '';
              setMessages((prev) => {
                const updated = [...prev];
                const orig = updated[updated.length - 1];
                if (orig && orig.agentData?.completion_reason === 'in_progress') {
                  const lastMsg = { ...orig };
                  lastMsg.content = finalContent;
                  lastMsg.agentData = {
                    ...lastMsg.agentData!,
                    completion_reason: completeData.completion_reason || 'complete',
                    iterations: completeData.iterations ?? lastMsg.agentData!.iterations,
                    tool_calls_made:
                      completeData.tool_calls_made ?? lastMsg.agentData!.tool_calls_made,
                  };
                  updated[updated.length - 1] = lastMsg;
                }
                return updated;
              });
            } else if (data.type === 'error') {
              const errorMsg = data.data?.message || data.content || 'Agent execution failed';
              setMessages((prev) => {
                const updated = [...prev];
                const orig = updated[updated.length - 1];
                if (orig && orig.agentData?.completion_reason === 'in_progress') {
                  const lastMsg = { ...orig };
                  lastMsg.content = errorMsg;
                  lastMsg.agentData = {
                    ...lastMsg.agentData!,
                    completion_reason: 'error',
                  };
                  updated[updated.length - 1] = lastMsg;
                }
                return updated;
              });
            } else if (data.type === 'done') {
              cleanupReconnect();
            }
          } catch {
            // ignore parse errors
          }
        };
        eventSource.onerror = () => {
          eventSource.close();
          activeEventSource = null;
          if (cancelled) return;

          // SSE failed but the task may still be running.
          // Poll getActiveTask every 3s — keep thinking indicator until
          // the task finishes, then clean up.
          const pollId = setInterval(async () => {
            if (cancelled) {
              clearInterval(pollId);
              return;
            }
            try {
              const still = await chatApi.getActiveTask(
                projectId.toString(),
                currentChatId || undefined
              );
              if (!still?.task_id) {
                clearInterval(pollId);
                cleanupReconnect();
                setMessages((prev) => prev.filter((m) => m.id !== thinkingId));
              }
            } catch {
              // Task endpoint failed — assume task is gone
              clearInterval(pollId);
              cleanupReconnect();
              setMessages((prev) => prev.filter((m) => m.id !== thinkingId));
            }
          }, 3000);
        };
      } catch (err) {
        // No active task — expected when nothing is running
        console.debug('[CHAT] checkActiveTask:', err);
      }
    };

    // Sequential: load history first, then check for active task
    loadChatHistory().then((msgs) => {
      if (!cancelled) checkActiveTask(msgs);
    });

    // Cleanup on unmount or dependency change
    return () => {
      cancelled = true;
      if (reconnectTimeoutId) clearTimeout(reconnectTimeoutId);
      if (activeEventSource) {
        activeEventSource.close();
        activeEventSource = null;
      }
      // Abort any in-flight agent streaming from the previous project
      if (abortControllerRef.current) {
        abortControllerRef.current.abort();
        abortControllerRef.current = null;
      }
    };
  }, [projectId, initialAgents, currentChatId]);

  // Load chat sessions for multi-session support
  useEffect(() => {
    const loadSessions = async () => {
      try {
        const sessionList = await chatApi.getProjectSessions(projectId.toString());
        setSessions(sessionList);
        // Set current chat to the most recent session
        if (sessionList.length > 0 && !currentChatId) {
          setCurrentChatId(sessionList[0].id);
        }
      } catch {
        // ignore
      }
    };
    loadSessions();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  // Update agents when initialAgents prop changes
  useEffect(() => {
    if (initialAgents.length > 0) {
      setAgents(initialAgents);

      // Set first agent as default if current agent not in list
      if (!initialAgents.find((a) => a.id === currentAgent.id)) {
        const defaultAgent = initialAgents[0];
        setCurrentAgent(defaultAgent);
        onSelectAgent(defaultAgent);
      }
    }
  }, [initialAgents, currentAgent.id, onSelectAgent]);

  // Fetch installed skills for the current agent (for slash command autocomplete)
  useEffect(() => {
    if (!currentAgent.backendId) {
      setAvailableSkills([]);
      return;
    }
    let cancelled = false;
    marketplaceApi
      .getAgentSkills(currentAgent.backendId.toString())
      .then((data) => {
        if (!cancelled) {
          setAvailableSkills(
            (data.skills || []).map((s: { name: string; description: string }) => ({
              name: s.name,
              description: s.description,
            }))
          );
        }
      })
      .catch(() => {
        if (!cancelled) setAvailableSkills([]);
      });
    return () => {
      cancelled = true;
    };
  }, [currentAgent.backendId]);

  // Fetch active MCP servers for current agent
  useEffect(() => {
    if (!currentAgent.backendId) {
      setActiveMcpServers([]);
      return;
    }
    let cancelled = false;
    marketplaceApi
      .getAgentMcpServers(currentAgent.backendId.toString())
      .then((data) => {
        if (!cancelled) {
          setActiveMcpServers(
            (data || []).map((s: { server_name?: string; server_slug?: string }) => ({
              name: s.server_name || s.server_slug || 'MCP',
              slug: s.server_slug || '',
            }))
          );
        }
      })
      .catch(() => {
        if (!cancelled) setActiveMcpServers([]);
      });
    return () => {
      cancelled = true;
    };
  }, [currentAgent.backendId]);

  // WebSocket connection with auto-reconnect and heartbeat
  useEffect(() => {
    const token = localStorage.getItem('token');
    if (!token) return;

    let ws: WebSocket | null = null;
    let isCleaningUp = false;
    let reconnectAttempts = 0;
    let reconnectTimer: NodeJS.Timeout | null = null;
    let heartbeatTimer: NodeJS.Timeout | null = null;
    const maxReconnectAttempts = 10;
    const baseReconnectDelay = 1000;
    const heartbeatInterval = 30000; // 30 seconds

    const startHeartbeat = () => {
      if (heartbeatTimer) {
        clearInterval(heartbeatTimer);
      }

      heartbeatTimer = setInterval(() => {
        if (ws && ws.readyState === WebSocket.OPEN) {
          try {
            ws.send(JSON.stringify({ type: 'ping', project_id: projectId }));
            console.log('[WS] Heartbeat ping sent');
          } catch (error) {
            console.error('[WS] Heartbeat error:', error);
          }
        }
      }, heartbeatInterval);
    };

    const stopHeartbeat = () => {
      if (heartbeatTimer) {
        clearInterval(heartbeatTimer);
        heartbeatTimer = null;
      }
    };

    const connectWebSocket = () => {
      if (isCleaningUp) return;

      if (wsRef.current && wsRef.current.readyState !== WebSocket.CLOSED) {
        wsRef.current.close();
      }

      try {
        ws = createWebSocket(token);
        wsRef.current = ws;

        ws.onopen = () => {
          if (isCleaningUp) return;

          console.log('[WS] WebSocket connected');
          reconnectAttempts = 0;
          startHeartbeat();
        };

        ws.onmessage = (event) => {
          if (isCleaningUp || !isMountedRef.current) return;

          const data = JSON.parse(event.data);

          // Handle pong response
          if (data.type === 'pong') {
            console.log('[WS] Heartbeat pong received');
            return;
          }

          console.log('[WS] Message:', data.type);

          if (data.type === 'stream') {
            setCurrentStream((prev) => prev + data.content);

            // Extract file names from code blocks
            const codeBlockPattern = /```\w+\s*\n\/\/\s*File:\s*([^\n]+)/g;
            let match;
            while ((match = codeBlockPattern.exec(data.content)) !== null) {
              const fileName = match[1].trim();
              setStreamingFiles((prev) =>
                new Map(prev).set(fileName, { fileName, isStreaming: true })
              );
            }
          } else if (data.type === 'complete') {
            // Handle complete event from both StreamAgent and IterativeAgent
            const finalResponse = data.data?.final_response || data.content || currentStream;

            setMessages((prev) => [
              ...prev,
              {
                id: `msg-${Date.now()}`,
                type: 'ai',
                content: finalResponse,
              },
            ]);
            setCurrentStream('');
            setIsStreaming(false);
            setStreamingFiles((prev) => {
              const newMap = new Map(prev);
              newMap.forEach((file, key) => {
                newMap.set(key, { ...file, isStreaming: false });
              });
              return newMap;
            });
          } else if (data.type === 'file_ready') {
            onFileUpdate(data.file_path, data.content);
            toast.success(`Created ${data.file_path}`, { duration: 2000 });

            const fileName = data.file_path.replace(/^src\//, '');
            setStreamingFiles((prev) => {
              const newMap = new Map(prev);
              if (newMap.has(fileName)) {
                newMap.set(fileName, { fileName, isStreaming: false });
              }
              return newMap;
            });
          } else if (data.type === 'error') {
            toast.error(data.content);
            setIsStreaming(false);
            setCurrentStream('');
            setStreamingFiles((prev) => {
              const newMap = new Map(prev);
              newMap.forEach((file, key) => {
                newMap.set(key, { ...file, isStreaming: false });
              });
              return newMap;
            });
          } else if (data.type === 'approval_required') {
            // Auto-approve if user has switched to "Allow All Edits" mode
            if (editModeRef.current === 'allow') {
              if (ws && ws.readyState === WebSocket.OPEN) {
                ws.send(
                  JSON.stringify({
                    type: 'approval_response',
                    approval_id: data.data.approval_id,
                    response: 'allow_all',
                  })
                );
              }
            } else {
              // Show approval prompt in "Ask" mode
              const approvalMessage: Message = {
                id: `approval-${Date.now()}`,
                type: 'approval_request',
                content: '',
                approvalId: data.data.approval_id,
                toolName: data.data.tool_name,
                toolParameters: data.data.tool_parameters,
                toolDescription: data.data.tool_description,
              };
              setMessages((prev) => [...prev, approvalMessage]);
            }
          } else if (data.type === 'agent_event') {
            // Cross-pod agent event forwarded via Pub/Sub bridge.
            // Filter by chat_id to prevent bleeding from other sessions.
            if (data.chat_id && data.chat_id !== currentChatIdRef.current) {
              return;
            }
          } else if (data.type === 'status_update') {
            // Handle lifecycle status updates
            const payload = data.payload || data.data;
            const eventType = payload?.type;
            const status = payload?.environment_status || payload?.status;
            const message = payload?.message;

            // New lifecycle events (type-based)
            if (eventType === 'idle_warning') {
              onIdleWarning?.(payload?.minutes_until_shutdown ?? 5);
            } else if (eventType === 'environment_stopping') {
              onEnvironmentStopping?.();
              toast.loading(message || 'Stopping environment...', {
                id: 'env-stopping',
                duration: 10000,
              });
            } else if (eventType === 'environment_stopped') {
              toast.dismiss('env-stopping');
              onEnvironmentStopped?.(payload?.reason || 'unknown');
              toast(message || 'Environment stopped', { icon: '\u23F8\uFE0F', duration: 5000 });
            } else if (eventType === 'volume_restoring') {
              toast.loading(message || 'Restoring project files...', {
                id: 'volume-restore',
                duration: 30000,
              });
            } else if (eventType === 'volume_ready') {
              toast.dismiss('volume-restore');
              toast.success('Environment ready', { duration: 2000 });
              onVolumeReady?.();
              // Legacy status-based events
            } else if (status === 'hibernating') {
              toast.loading(message || 'Project is being saved...', { duration: 5000 });
              setTimeout(() => {
                navigate('/projects');
              }, 2000);
            } else if (status === 'hibernated') {
              toast.success(message || 'Project saved successfully');
              navigate('/projects');
            } else if (status === 'waking') {
              toast.loading(message || 'Waking up project...', { duration: 10000 });
            } else if (status === 'active') {
              toast.success(message || 'Project is ready!');
              toast.dismiss('volume-restore');
            } else if (status === 'corrupted') {
              toast.error(message || 'Project data may be corrupted');
            }
          }
        };

        ws.onerror = (error) => {
          if (!isCleaningUp) {
            console.error('[WS] WebSocket error:', error);
          }
        };

        ws.onclose = () => {
          if (isCleaningUp) return;

          console.log('[WS] WebSocket disconnected');
          stopHeartbeat();

          // Attempt to reconnect with exponential backoff
          if (reconnectAttempts < maxReconnectAttempts) {
            const delay = Math.min(baseReconnectDelay * Math.pow(2, reconnectAttempts), 30000);
            reconnectAttempts++;

            console.log(
              `[WS] Reconnecting in ${delay}ms (attempt ${reconnectAttempts}/${maxReconnectAttempts})`
            );

            reconnectTimer = setTimeout(() => {
              connectWebSocket();
            }, delay);
          } else {
            console.error('[WS] Max reconnect attempts reached');
            toast.error('Connection lost. Please refresh the page.', { duration: 5000 });
          }
        };
      } catch (error) {
        console.error('[WS] Failed to create WebSocket:', error);
      }
    };

    connectWebSocket();

    return () => {
      isCleaningUp = true;
      stopHeartbeat();

      if (reconnectTimer) {
        clearTimeout(reconnectTimer);
      }

      if (ws && ws.readyState !== WebSocket.CLOSED) {
        ws.close();
      }
    };
    // Only reconnect when projectId changes, not when onFileUpdate changes
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  // Track desktop/mobile state using matchMedia - no reflows, no debounce needed
  useEffect(() => {
    if (typeof window === 'undefined' || !window.matchMedia) return;

    const mediaQuery = window.matchMedia('(min-width: 768px)');

    // Update state when media query changes
    const handleChange = (e: MediaQueryListEvent) => {
      setIsDesktop(e.matches);
    };

    // Modern browsers use addEventListener
    mediaQuery.addEventListener('change', handleChange);
    return () => {
      mediaQuery.removeEventListener('change', handleChange);
    };
  }, []);

  // Track user scroll behavior
  useEffect(() => {
    const container = messagesContainerRef.current;
    if (!container) return;

    const handleScroll = () => {
      const { scrollTop, scrollHeight, clientHeight } = container;
      const isNearBottom = scrollHeight - scrollTop - clientHeight < 100;

      // User is scrolling up if not near bottom
      isUserScrollingRef.current = !isNearBottom;
    };

    container.addEventListener('scroll', handleScroll);
    return () => container.removeEventListener('scroll', handleScroll);
  }, []);

  // Smart auto-scroll: only scroll if user hasn't manually scrolled up
  useEffect(() => {
    const container = messagesContainerRef.current;
    if (!effectiveIsExpanded || !container || !messagesEndRef.current) return;

    const { scrollTop, scrollHeight, clientHeight } = container;
    const isNearBottom = scrollHeight - scrollTop - clientHeight < 100;

    // Only auto-scroll if:
    // 1. User hasn't manually scrolled up (isUserScrollingRef is false)
    // 2. OR user is already near the bottom
    // 3. OR this is a new user message (messages array grew and last message is user type)
    const lastMessage = messages[messages.length - 1];
    const isNewUserMessage = lastMessage?.type === 'user';

    if (isNewUserMessage || !isUserScrollingRef.current || isNearBottom) {
      messagesEndRef.current.scrollIntoView({ behavior: 'smooth' });
      isUserScrollingRef.current = false; // Reset after scrolling
    }
  }, [messages, currentStream, effectiveIsExpanded]);

  // Collapse chat when clicking outside (including clicks on iframe/preview) - desktop only
  // Skip this behavior when docked since docked chat should always stay open
  useEffect(() => {
    // Don't add collapse behavior when docked
    if (isDocked) return;

    const handleClickOutside = (event: MouseEvent) => {
      // Only auto-close on desktop - use cached isDesktop state to avoid forced reflow
      if (
        isDesktop &&
        containerRef.current &&
        !containerRef.current.contains(event.target as Node)
      ) {
        setIsExpanded(false);
      }
    };

    const handleWindowBlur = () => {
      // Close chat when user clicks on iframe (preview window) - desktop only
      // Only collapse if pointer is actually over the preview (user clicked it),
      // not when a programmatic iframe.src refresh triggers window blur.
      if (isDesktop) {
        setTimeout(() => {
          if (
            document.activeElement?.tagName === 'IFRAME' &&
            isExpanded &&
            isPointerOverPreviewRef?.current
          ) {
            setIsExpanded(false);
          }
        }, 0);
      }
    };

    if (isExpanded) {
      document.addEventListener('mousedown', handleClickOutside);
      window.addEventListener('blur', handleWindowBlur);
      return () => {
        document.removeEventListener('mousedown', handleClickOutside);
        window.removeEventListener('blur', handleWindowBlur);
      };
    }
  }, [isExpanded, isDocked, isDesktop, isPointerOverPreviewRef]);

  const handleInputFocus = () => {
    setIsExpanded(true);
  };

  const handleAgentSelect = (agent: ChatAgent) => {
    setCurrentAgent(agent);
    onSelectAgent(agent);
  };

  const handleModelChange = async (model: string) => {
    // Read current values before optimistic update to enable revert
    const agentBackendId = currentAgent.backendId;
    const previousModel = currentAgent.selectedModel;

    // Optimistic update
    setCurrentAgent((prev) => ({ ...prev, selectedModel: model }));

    try {
      if (agentBackendId) {
        await marketplaceApi.selectAgentModel(String(agentBackendId), model);
      }
      toast.success(`Model changed to ${model}`, { duration: 2000 });
    } catch (error) {
      console.error('Failed to change model:', error);
      // Revert on failure — only if agent hasn't changed in the meantime
      setCurrentAgent((prev) =>
        prev.backendId === agentBackendId ? { ...prev, selectedModel: previousModel } : prev
      );
      toast.error('Failed to change model');
    }
  };

  const sendStreamMessage = (message: string) => {
    if (!message.trim() || !wsRef.current || isStreaming) return;

    const userMessage: Message = {
      id: `msg-${Date.now()}`,
      type: 'user',
      content: message,
      timestamp: new Date().toISOString(),
    };
    setMessages((prev) => [...prev, userMessage]);
    setIsStreaming(true);
    setStreamingFiles(new Map());

    wsRef.current.send(
      JSON.stringify({
        message,
        project_id: projectId,
        container_id: containerId, // Container ID for scoped file access
        chat_id: currentChatId, // Target specific chat session
        agent_id: currentAgent.backendId, // Include agent_id
        edit_mode: editMode, // Include edit mode
        view_context: viewContext, // UI view context for scoped tools
      })
    );
  };

  const abortControllerRef = useRef<AbortController | null>(null);
  const escPressCountRef = useRef(0);
  const escTimeoutRef = useRef<NodeJS.Timeout | null>(null);

  const stopAgentExecution = useCallback(async () => {
    // Stop-as-undo: unblock the input IMMEDIATELY. The server's lock-takeover
    // path lets the next send steal the lock from this cancelled task, so we
    // don't wait for the cancel RPC to ACK.
    const controller = abortControllerRef.current;
    if (controller) {
      controller.abort();
      abortControllerRef.current = null;
    }
    const taskId = agentTaskIdRef.current;
    agentTaskIdRef.current = null;
    setAgentExecuting(false);
    if (currentChatIdRef.current) {
      agentRuns.markTerminal(currentChatIdRef.current, 'superseded');
    }

    if (taskId) {
      // Fire-and-forget — input is already ready for the next message.
      chatApi.cancelAgentTask(taskId).catch(() => {
        // Swallow: task may already be done; the takeover path handles any
        // stuck lock on the next send.
      });
    }
  }, [agentRuns]);

  // ESC key handler for stopping execution
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && agentExecuting) {
        escPressCountRef.current += 1;
        const newCount = escPressCountRef.current;

        // Clear previous timeout
        if (escTimeoutRef.current) {
          clearTimeout(escTimeoutRef.current);
        }

        // Reset count after 500ms
        escTimeoutRef.current = setTimeout(() => {
          escPressCountRef.current = 0;
        }, 500);

        // Stop execution on double ESC
        if (newCount >= 2) {
          stopAgentExecution();
          escPressCountRef.current = 0;
          toast.success('Agent stopped (ESC pressed twice)');
        } else {
          toast('Press ESC again to stop agent', { duration: 500 });
        }
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
      if (escTimeoutRef.current) {
        clearTimeout(escTimeoutRef.current);
      }
    };
  }, [agentExecuting, stopAgentExecution]);

  const sendAgentMessage = async (
    message: string,
    attachments?: SerializedAttachment[],
    mentions?: ChatMention[]
  ) => {
    if ((!message.trim() && (!attachments || attachments.length === 0)) || agentExecuting) return;

    const userMessage: Message = {
      id: `msg-${Date.now()}`,
      type: 'user',
      content: message,
      timestamp: new Date().toISOString(),
      attachments,
    };
    setMessages((prev) => [...prev, userMessage]);
    setAgentExecuting(true);

    // Create abort controller
    const controller = new AbortController();
    abortControllerRef.current = controller;

    // Create initial "thinking" message
    const thinkingMessageId = `msg-${Date.now()}-thinking`;
    const thinkingMessage: Message = {
      id: thinkingMessageId,
      type: 'ai',
      content: '',
      agentData: {
        steps: [],
        iterations: 0,
        tool_calls_made: 0,
        completion_reason: 'in_progress',
      },
      agentIcon: currentAgent.icon,
      agentAvatarUrl: currentAgent.avatar_url,
      agentType: currentAgent.name,
    };
    setMessages((prev) => [...prev, thinkingMessage]);

    try {
      await chatApi.sendAgentMessageStreaming(
        {
          project_id: projectId.toString(),
          container_id: containerId, // Container ID for scoped file access
          chat_id: currentChatId || undefined, // Target specific chat session
          message,
          agent_id: currentAgent.backendId?.toString(),
          max_iterations: undefined,
          edit_mode: editMode,
          view_context: viewContext, // UI view context for scoped tools
          attachments,
          // @-mention picker entries — backend splits these by kind into
          // mention_agent_ids / mention_mcp_config_ids /
          // mention_app_instance_ids on the AgentTaskPayload.
          mentions: mentions && mentions.length > 0 ? mentions : undefined,
        },
        (event) => {
          // Guard against state updates after unmount (orphaned SSE callbacks)
          if (!isMountedRef.current) return;

          notifyFileMutationsFromAgentEvent(event);

          // Capture task ID from streaming events for cancellation/reconnection
          if (event.data?.task_id) {
            agentTaskIdRef.current = event.data.task_id as string;
            // Publish to AgentRunsContext so other parts of the UI (sidebar
            // dots, multi-chat view) can show this chat as running.
            if (currentChatIdRef.current) {
              agentRuns.register(currentChatIdRef.current, event.data.task_id as string);
            }
          }

          if (event.type === 'text_delta') {
            // Stream LLM text into a visible message, not the thinking indicator
            const deltaContent = (event.data?.content as string) || '';
            if (deltaContent) {
              setMessages((prev) => {
                const updated = [...prev];
                // Find the last non-thinking message to append to
                const nonThinking = updated.filter((m) => m.id !== thinkingMessageId);
                const lastMsg = nonThinking[nonThinking.length - 1];

                if (lastMsg && lastMsg.type === 'ai' && !lastMsg.agentData?.steps?.length) {
                  // Append to existing streaming message
                  const idx = updated.indexOf(lastMsg);
                  updated[idx] = { ...lastMsg, content: (lastMsg.content || '') + deltaContent };
                } else {
                  // Create new streaming message before thinking indicator
                  const streamMsg: Message = {
                    id: `${thinkingMessageId}-stream`,
                    type: 'ai' as const,
                    content: deltaContent,
                    agentIcon: currentAgent.icon,
                    agentAvatarUrl: currentAgent.avatar_url,
                    agentType: currentAgent.name,
                  };
                  const thinkingIdx = updated.findIndex((m) => m.id === thinkingMessageId);
                  if (thinkingIdx >= 0) {
                    updated.splice(thinkingIdx, 0, streamMsg);
                  } else {
                    updated.push(streamMsg);
                  }
                }
                return updated;
              });
            }
          } else if (event.type === 'tool_call') {
            // Per-tool streaming — accumulate into ONE message per iteration
            const tc = event.data as Record<string, unknown>;
            const iterMsgId = `${thinkingMessageId}-iter-${tc.iteration as number}`;
            const newTool: import('../../types/agent').ToolCallDetail = {
              name: tc.name as string,
              parameters: (tc.parameters as Record<string, unknown>) || {},
              result: tc.result as import('../../types/agent').ToolCallDetail['result'],
            };

            setMessages((prev) => {
              const withoutThinking = prev.filter((m) => m.id !== thinkingMessageId);
              const existingIdx = withoutThinking.findIndex((m) => m.id === iterMsgId);

              if (existingIdx >= 0) {
                // APPEND tool to existing iteration message
                const existing = withoutThinking[existingIdx];
                const currentTools = existing.agentData?.steps?.[0]?.tool_calls || [];
                withoutThinking[existingIdx] = {
                  ...existing,
                  agentData: {
                    ...existing.agentData!,
                    steps: [
                      {
                        ...existing.agentData!.steps[0],
                        tool_calls: [...currentTools, newTool],
                      } as import('../../types/agent').AgentStep,
                    ],
                    tool_calls_made: currentTools.length + 1,
                  },
                };
              } else {
                // CREATE iteration message with first tool
                withoutThinking.push({
                  id: iterMsgId,
                  type: 'ai',
                  content: '',
                  agentData: {
                    steps: [{ tool_calls: [newTool] } as import('../../types/agent').AgentStep],
                    iterations: (tc.iteration as number) || 0,
                    tool_calls_made: 1,
                    completion_reason: 'tool_streaming',
                  },
                  agentIcon: currentAgent.icon,
                  agentAvatarUrl: currentAgent.avatar_url,
                  agentType: currentAgent.name,
                });
              }

              return [...withoutThinking, { ...thinkingMessage, id: thinkingMessageId }];
            });

            const tcResult = tc.result as Record<string, unknown> | undefined;
            if (
              tc.name === 'kanban' &&
              (tcResult?.result as Record<string, unknown> | undefined)?.success !== false
            ) {
              window.dispatchEvent(new CustomEvent('kanban-updated'));
            }
          } else if (event.type === 'agent_step') {
            // Finalize iteration message with canonical data from backend
            const stepData = event.data as Record<string, unknown>;
            const iterMsgId = `${thinkingMessageId}-iter-${stepData.iteration as number}`;
            const rawToolCalls =
              (stepData.tool_calls as Array<{ name: string; parameters: unknown }> | undefined) ||
              [];
            const rawToolResults =
              (stepData.tool_results as Array<Record<string, unknown>> | undefined) || [];
            const transformedToolCalls: import('../../types/agent').ToolCallDetail[] =
              rawToolCalls.map((tc, index) => ({
                name: tc.name,
                parameters: (tc.parameters as Record<string, unknown>) || {},
                result:
                  (rawToolResults[index] as import('../../types/agent').ToolCallDetail['result']) ||
                  undefined,
              }));
            const {
              tool_results: _tool_results,
              tool_calls: _tool_calls,
              ...restStepData
            } = stepData;
            const transformedStep: import('../../types/agent').AgentStep = {
              ...(restStepData as Omit<import('../../types/agent').AgentStep, 'tool_calls'>),
              tool_calls: transformedToolCalls,
            };

            const stepMessage: Message = {
              id: iterMsgId,
              type: 'ai',
              content: '',
              agentData: {
                steps: [transformedStep],
                iterations: (stepData.iteration as number) || 0,
                tool_calls_made: rawToolCalls.length,
                completion_reason: 'step_complete',
              },
              agentIcon: currentAgent.icon,
              agentAvatarUrl: currentAgent.avatar_url,
              agentType: currentAgent.name,
            };

            setMessages((prev) => {
              const withoutThinking = prev.filter((m) => m.id !== thinkingMessageId);
              const existingIdx = withoutThinking.findIndex((m) => m.id === iterMsgId);
              if (existingIdx >= 0) {
                withoutThinking[existingIdx] = stepMessage;
              } else {
                withoutThinking.push(stepMessage);
              }
              return [...withoutThinking, { ...thinkingMessage, id: thinkingMessageId }];
            });

            // Side effects
            const toolCalls = rawToolCalls;
            const toolResults = rawToolResults;
            if (
              toolCalls.some(
                (tc: { name: string }, idx: number) =>
                  tc.name === 'kanban' &&
                  (toolResults[idx]?.result as Record<string, unknown> | undefined)?.success !==
                    false
              )
            ) {
              window.dispatchEvent(new CustomEvent('kanban-updated'));
            }
          } else if (event.type === 'complete') {
            // Remove thinking message
            setMessages((prev) => prev.filter((msg) => msg.id !== thinkingMessageId));

            if (event.data.success === false) {
              const errorDetail = event.data.error
                ? formatAgentError(event.data.error as string)
                : 'Agent could not complete the task';

              // Add error message to chat so the user sees inline feedback
              setMessages((prev) => {
                const lastMsg = prev[prev.length - 1];
                const errorContent = `I encountered an error: ${errorDetail}`;
                if (lastMsg && lastMsg.agentData) {
                  return [
                    ...prev.slice(0, -1),
                    {
                      ...lastMsg,
                      content: errorContent,
                      agentData: {
                        ...lastMsg.agentData,
                        completion_reason: 'error',
                      },
                    },
                  ];
                }
                return [
                  ...prev,
                  {
                    id: `msg-${Date.now()}-error`,
                    type: 'ai',
                    content: errorContent,
                    agentData: {
                      steps: [],
                      iterations: (event.data.iterations as number) || 0,
                      tool_calls_made: (event.data.tool_calls_made as number) || 0,
                      completion_reason: 'error',
                    },
                    agentIcon: currentAgent.icon,
                    agentAvatarUrl: currentAgent.avatar_url,
                    agentType: currentAgent.name,
                  },
                ];
              });

              toast.error(errorDetail, { duration: 5000 });
            } else {
              // Add final response as part of AgentMessage (not a separate message)
              const finalContent = event.data.final_response as string | undefined;
              if (finalContent && finalContent.trim()) {
                // Update the last agent message to include the final response
                setMessages((prev) => {
                  const lastMsg = prev[prev.length - 1];
                  if (lastMsg && lastMsg.agentData) {
                    return [
                      ...prev.slice(0, -1),
                      {
                        ...lastMsg,
                        content: finalContent ?? '',
                      },
                    ];
                  }
                  // Fallback: if no agent message exists, create one
                  return [
                    ...prev,
                    {
                      id: `msg-${Date.now()}-result`,
                      type: 'ai',
                      content: finalContent ?? '',
                      agentData: {
                        steps: [],
                        iterations: 0,
                        tool_calls_made: 0,
                        completion_reason: 'complete',
                      },
                      agentIcon: currentAgent.icon,
                      agentAvatarUrl: currentAgent.avatar_url,
                      agentType: currentAgent.name,
                    },
                  ];
                });
              }

              toast.success('Task completed successfully');
            }
          } else if (event.type === 'credits_used') {
            // Dispatch custom event so UserDropdown and other UI can update
            window.dispatchEvent(
              new CustomEvent('credits-updated', {
                detail: {
                  newBalance: event.data.new_balance,
                  creditsUsed: event.data.credits_deducted,
                  costTotal: event.data.cost_total,
                },
              })
            );
          } else if (event.type === 'tool_error') {
            // Non-fatal: single tool call failed, agent continues.
            const toolErr = event.data as { tool_name?: string; error?: string };
            const toolLabel = toolErr.tool_name ? `[${toolErr.tool_name}] ` : '';
            toast.error(`${toolLabel}${toolErr.error || 'Tool call failed'}`, {
              duration: 5000,
              id: `tool-err-${toolErr.tool_name ?? 'unknown'}`,
            });
          } else if (event.type === 'error') {
            const errorData = event.data as Record<string, unknown>;
            // Handle insufficient credits specifically
            if (errorData.code === 'insufficient_credits') {
              toast.error(
                (errorData.message as string | undefined) ||
                  'Insufficient credits. Please purchase more to continue.',
                { duration: 6000 }
              );
              setMessages((prev) => prev.filter((msg) => msg.id !== thinkingMessageId));
              return;
            }
            const errorMsg =
              (event as unknown as { content?: string }).content ||
              (errorData.message as string | undefined) ||
              'Agent execution failed';
            throw new Error(errorMsg);
          } else if (event.type === 'approval_required') {
            // Auto-approve if user has switched to "Allow All Edits" mode
            if (editModeRef.current === 'allow') {
              chatApi
                .sendApprovalResponse(event.data.approval_id as string, 'allow_all')
                .catch((err) => console.error('[APPROVAL] Auto-approve failed:', err));
            } else {
              // Show approval prompt in "Ask" mode
              const approvalMessage: Message = {
                id: `approval-${Date.now()}`,
                type: 'approval_request',
                content: '',
                approvalId: event.data.approval_id as string,
                toolName: event.data.tool_name as string,
                toolParameters: event.data.tool_parameters as Record<string, unknown>,
                toolDescription: event.data.tool_description as string,
              };
              setMessages((prev) => [...prev, approvalMessage]);
            }
          }
        },
        controller.signal
      );
    } catch (error: unknown) {
      if (error instanceof Error && error.name === 'AbortError') {
        console.log('[AGENT] Execution aborted by user');

        // Remove thinking message and mark last agent message as cancelled
        setMessages((prev) => {
          const withoutThinking = prev.filter((msg) => msg.id !== thinkingMessageId);
          const lastIdx = withoutThinking.length - 1;
          if (lastIdx >= 0 && withoutThinking[lastIdx].agentData) {
            withoutThinking[lastIdx] = {
              ...withoutThinking[lastIdx],
              content: withoutThinking[lastIdx].content || '_Execution stopped by user_',
              agentData: {
                ...withoutThinking[lastIdx].agentData!,
                completion_reason: 'cancelled',
              },
            };
            return withoutThinking;
          }
          return [
            ...withoutThinking,
            {
              id: `msg-${Date.now()}-stopped`,
              type: 'ai' as const,
              content: '_Execution stopped by user_',
            },
          ];
        });

        return;
      }

      console.error('[AGENT] Streaming execution error:', error);

      const errorDetail = error instanceof Error ? error.message : 'Failed to execute agent';

      // If an agent is already running (e.g. page was refreshed mid-execution),
      // reconnect to it instead of showing an error.
      if (typeof errorDetail === 'string' && errorDetail.includes('Another agent is running')) {
        toast('Agent is still processing a previous request. Please wait for it to finish.', {
          duration: 4000,
        });
        // Trigger reconnection by checking for the active task
        try {
          const activeTask = await chatApi.getActiveTask(
            projectId.toString(),
            currentChatId || undefined
          );
          if (activeTask?.task_id) {
            agentTaskIdRef.current = activeTask.task_id;
            // Add thinking placeholder
            setMessages((prev) => [
              ...prev.filter((m) => m.id !== thinkingMessageId),
              {
                id: `reconnect-${activeTask.task_id}`,
                type: 'ai' as const,
                content: '',
                agentData: {
                  steps: [],
                  iterations: 0,
                  tool_calls_made: 0,
                  completion_reason: 'in_progress' as const,
                },
              },
            ]);
            // Keep agentExecuting=true so send is blocked; the finally
            // block below is skipped via the early return.
            const eventSource = chatApi.subscribeToTask(activeTask.task_id);
            eventSource.onmessage = (event) => {
              try {
                const data = JSON.parse(event.data);
                if (data.type === 'agent_step') {
                  setMessages((prev) => {
                    const updated = [...prev];
                    const orig = updated[updated.length - 1] as Message | undefined;
                    if (orig?.agentData?.completion_reason === 'in_progress') {
                      const lastMsg = { ...orig } as Message;
                      lastMsg.agentData = {
                        ...lastMsg.agentData!,
                        steps: [
                          ...(lastMsg.agentData!.steps || []),
                          data.data as import('../../types/agent').AgentStep,
                        ],
                      };
                      updated[updated.length - 1] = lastMsg;
                    }
                    return updated;
                  });
                } else if (data.type === 'complete' || data.type === 'done') {
                  if (data.type === 'complete') {
                    const completePayload = data.data as Record<string, unknown> | undefined;
                    const finalContent =
                      (completePayload?.final_response as string | undefined) || '';
                    setMessages((prev) => {
                      const updated = [...prev];
                      const orig = updated[updated.length - 1] as Message | undefined;
                      if (orig?.agentData?.completion_reason === 'in_progress') {
                        const lastMsg = { ...orig } as Message;
                        lastMsg.content = finalContent;
                        lastMsg.agentData = {
                          ...lastMsg.agentData!,
                          completion_reason:
                            (completePayload?.completion_reason as string | undefined) ||
                            'complete',
                        };
                        updated[updated.length - 1] = lastMsg;
                      }
                      return updated;
                    });
                  }
                  eventSource.close();
                  setAgentExecuting(false);
                  agentTaskIdRef.current = null;
                  refreshSessions();
                } else if (data.type === 'error') {
                  const errPayload = data.data as Record<string, unknown> | undefined;
                  const errMsg =
                    (errPayload?.message as string | undefined) || 'Agent execution failed';
                  setMessages((prev) => {
                    const updated = [...prev];
                    const orig = updated[updated.length - 1] as Message | undefined;
                    if (orig?.agentData?.completion_reason === 'in_progress') {
                      const lastMsg = { ...orig } as Message;
                      lastMsg.content = errMsg;
                      lastMsg.agentData = { ...lastMsg.agentData!, completion_reason: 'error' };
                      updated[updated.length - 1] = lastMsg;
                    }
                    return updated;
                  });
                  eventSource.close();
                  setAgentExecuting(false);
                  agentTaskIdRef.current = null;
                }
              } catch {
                /* ignore parse errors */
              }
            };
            eventSource.onerror = () => {
              eventSource.close();
              setAgentExecuting(false);
              agentTaskIdRef.current = null;
            };
            return; // Skip the finally block — agentExecuting stays true
          }
        } catch {
          /* fallback to normal error display */
        }
      }

      // Remove thinking message and add error message
      setMessages((prev) => {
        const withoutThinking = prev.filter((msg) => msg.id !== thinkingMessageId);
        return [
          ...withoutThinking,
          {
            id: `msg-${Date.now()}-error`,
            type: 'ai',
            content:
              'I apologize, but I encountered an error while working on your request. The task could not be completed. Please try again or contact support if the issue persists.',
          },
        ];
      });

      toast.error(errorDetail, {
        duration: 5000,
      });
    } finally {
      setAgentExecuting(false);
      abortControllerRef.current = null;
      // Safety net: remove any leftover thinking message if the stream
      // ended without a terminal event (complete/error/abort).
      setMessages((prev) => prev.filter((msg) => msg.id !== thinkingMessageId));
      // Refresh sessions (fire-and-forget) to pick up new titles / status
      refreshSessions();
    }
  };

  const handleSendMessage = (
    message: string,
    attachments?: SerializedAttachment[],
    mentions?: ChatMention[]
  ) => {
    // Use agent's mode to determine stream vs agent execution. Mentions
    // only make sense for the agent path — stream mode bypasses the
    // worker / payload pipeline entirely, so we drop them silently.
    if (currentAgent.mode === 'agent') {
      sendAgentMessage(message, attachments, mentions);
    } else {
      sendStreamMessage(message);
    }
  };

  const handleClearHistory = async () => {
    try {
      const result = await chatApi.clearProjectMessages(
        projectId.toString(),
        currentChatId || undefined
      );
      setMessages([]);
      animatedMessagesRef.current.clear();
      toast.success(`Cleared ${result.deleted_count} messages`, { icon: '🗑️' });
    } catch (error) {
      console.error('[CHAT] Failed to clear history:', error);
      toast.error('Failed to clear chat history');
    }
  };

  const removeLastExchangeFromState = () => {
    setMessages((prev) => {
      const copy = [...prev];
      for (let i = copy.length - 1; i >= 0; i--) {
        if (copy[i].type === 'ai') {
          copy.splice(i, 1);
          break;
        }
      }
      for (let i = copy.length - 1; i >= 0; i--) {
        if (copy[i].type === 'user') {
          copy.splice(i, 1);
          break;
        }
      }
      return copy;
    });
  };

  const handleUndo = async () => {
    if (agentExecuting || !currentChatId) return;
    try {
      const result = await chatApi.undoLastExchange(currentChatId);
      if (!result.success || result.removed_count === 0) return;
      removeLastExchangeFromState();
      if (result.files_reverted) {
        toast.success('Changes reverted');
      }
    } catch (error: unknown) {
      console.error('[CHAT] Failed to undo:', error);
      const detail = (error as Record<string, Record<string, Record<string, string>>>)?.response
        ?.data?.detail;
      toast.error(detail || 'Failed to undo last message');
    }
  };

  const handleRetry = async () => {
    if (agentExecuting || !currentChatId) return;
    try {
      const result = await chatApi.undoLastExchange(currentChatId);
      if (!result.success || !result.last_user_message) return;
      removeLastExchangeFromState();
      sendAgentMessage(result.last_user_message);
    } catch (error: unknown) {
      console.error('[CHAT] Failed to retry:', error);
      const detail = (error as Record<string, Record<string, Record<string, string>>>)?.response
        ?.data?.detail;
      toast.error(detail || 'Failed to retry last message');
    }
  };

  const handleCompact = async () => {
    if (agentExecuting || !currentChatId) return;
    try {
      toast('Compacting context...');
      const result = await chatApi.compactChat(currentChatId);
      if (!result.success) {
        toast.error(result.detail || 'Could not compact');
        return;
      }
      // Clear chat and show summary as a fresh conversation
      setMessages([
        {
          id: `msg-compact-${Date.now()}`,
          type: 'ai',
          content: result.summary || 'Context compacted.',
          agentData: {
            steps: [],
            iterations: 0,
            tool_calls_made: 0,
            completion_reason: 'compact',
          },
          agentIcon: currentAgent.icon,
          agentAvatarUrl: currentAgent.avatar_url,
          agentType: currentAgent.name,
        },
      ]);
      animatedMessagesRef.current.clear();
      toast.success(`Compacted: ${result.before_count} → ${result.after_count} messages`);
    } catch (error) {
      console.error('[CHAT] Failed to compact:', error);
      toast.error('Failed to compact context');
    }
  };

  // Session management handlers
  const refreshSessions = useCallback(async () => {
    try {
      const sessionList = await chatApi.getProjectSessions(projectId.toString());
      setSessions(sessionList);
    } catch {
      // non-blocking
    }
  }, [projectId]);

  const handleNewSession = useCallback(async () => {
    try {
      const newChat = await chatApi.create(projectId.toString());
      setSessionTransitioning(true);
      setMessages([]);
      animatedMessagesRef.current.clear();
      setCurrentChatId(newChat.id);
      await refreshSessions();
      // Brief transition effect
      setTimeout(() => setSessionTransitioning(false), 200);
    } catch (error) {
      console.error('[CHAT] Failed to create session:', error);
      toast.error('Failed to create new chat session');
    }
  }, [projectId, refreshSessions]);

  const handleSelectSession = useCallback(
    (sessionId: string) => {
      if (sessionId === currentChatId) return;
      setSessionTransitioning(true);
      setMessages([]);
      animatedMessagesRef.current.clear();
      setCurrentChatId(sessionId);
      // Brief transition effect
      setTimeout(() => setSessionTransitioning(false), 200);
    },
    [currentChatId]
  );

  const handleRenameSession = useCallback(async (sessionId: string, newTitle: string) => {
    try {
      await chatApi.updateChatSession(sessionId, { title: newTitle });
      setSessions((prev) => prev.map((s) => (s.id === sessionId ? { ...s, title: newTitle } : s)));
    } catch (error) {
      console.error('[CHAT] Failed to rename session:', error);
      toast.error('Failed to rename session');
    }
  }, []);

  const handleDeleteSession = useCallback(
    async (sessionId: string) => {
      try {
        await chatApi.deleteChat(sessionId);
        // If we deleted the current chat, switch to the first remaining one
        if (sessionId === currentChatId) {
          const remaining = sessions.filter((s) => s.id !== sessionId);
          if (remaining.length > 0) {
            setSessionTransitioning(true);
            setMessages([]);
            animatedMessagesRef.current.clear();
            setCurrentChatId(remaining[0].id);
            setTimeout(() => setSessionTransitioning(false), 200);
          }
        }
        setSessions((prev) => prev.filter((s) => s.id !== sessionId));
      } catch (error) {
        console.error('[CHAT] Failed to delete session:', error);
        toast.error('Failed to delete chat session');
      }
    },
    [currentChatId, sessions]
  );

  // -------------------------------------------------------------------------
  // Command palette / keyboard wiring for chat actions.
  // -------------------------------------------------------------------------

  const cycleSession = useCallback(
    (delta: number) => {
      if (sessions.length === 0) return;
      const idx = sessions.findIndex((s) => s.id === currentChatId);
      const next = sessions[(idx + delta + sessions.length) % sessions.length];
      if (next && next.id !== currentChatId) handleSelectSession(next.id);
    },
    [sessions, currentChatId, handleSelectSession]
  );

  const cycleEditMode = useCallback(() => {
    setEditMode((prev) => {
      const order: EditMode[] = ['ask', 'plan', 'allow'];
      const i = order.indexOf(prev);
      return order[(i + 1) % order.length];
    });
  }, []);

  const renameCurrentSession = useCallback(() => {
    if (!currentChatId) return;
    const session = sessions.find((s) => s.id === currentChatId);
    const proposed = window.prompt('Rename session', session?.title ?? '');
    if (proposed && proposed.trim() && proposed !== session?.title) {
      handleRenameSession(currentChatId, proposed.trim());
    }
  }, [currentChatId, sessions, handleRenameSession]);

  const deleteCurrentSession = useCallback(() => {
    if (!currentChatId) return;
    if (window.confirm('Delete this chat session?')) {
      handleDeleteSession(currentChatId);
    }
  }, [currentChatId, handleDeleteSession]);

  useCommandHandlers({
    clearChat: () => {
      handleClearHistory();
    },
    newChatSession: handleNewSession,
    nextChatSession: () => cycleSession(1),
    prevChatSession: () => cycleSession(-1),
    stopAgent: () => {
      stopAgentExecution();
    },
    toggleEditMode: cycleEditMode,
    renameChatSession: renameCurrentSession,
    deleteChatSession: deleteCurrentSession,
    switchModel: () => {
      // The agent dropdown is rendered above the input — focus the chat
      // input first so the user is anchored, then dispatch a window event
      // that the dropdown listens for to open itself.
      window.dispatchEvent(new Event('tesslate:focus-chat'));
      window.dispatchEvent(new Event('tesslate:open-agent-picker'));
    },
    focusChatInput: () => {
      window.dispatchEvent(new Event('tesslate:focus-chat'));
    },
    attachChatFile: () => {
      window.dispatchEvent(new Event('tesslate:open-attach'));
    },
  });

  useHotkeys(
    'mod+shift+j',
    (e) => {
      e.preventDefault();
      handleNewSession();
    },
    { enableOnFormTags: false }
  );
  useHotkeys(
    'mod+]',
    (e) => {
      e.preventDefault();
      cycleSession(1);
    },
    { enableOnFormTags: false }
  );
  useHotkeys(
    'mod+[',
    (e) => {
      e.preventDefault();
      cycleSession(-1);
    },
    { enableOnFormTags: false }
  );
  useHotkeys(
    'mod+.',
    (e) => {
      e.preventDefault();
      if (agentExecuting) stopAgentExecution();
    },
    { enableOnFormTags: ['INPUT', 'TEXTAREA'] }
  );
  useHotkeys(
    'mod+shift+c',
    (e) => {
      e.preventDefault();
      window.dispatchEvent(new Event('tesslate:focus-chat'));
    },
    { enableOnFormTags: false }
  );

  // Get current session title for header
  const currentSessionTitle = useMemo(() => {
    if (!currentChatId || sessions.length === 0) return 'Agents';
    const session = sessions.find((s) => s.id === currentChatId);
    return session?.title || 'Untitled';
  }, [currentChatId, sessions]);

  // Find the last user message ID for the retry button
  const lastUserMessageId = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i].type === 'user') return messages[i].id;
    }
    return null;
  }, [messages]);

  const handleRetryMessage = (content: string) => {
    if (agentExecuting || isStreaming) return;
    handleSendMessage(content);
  };

  const handleApprovalResponse = async (
    approvalId: string,
    response: 'allow_once' | 'allow_all' | 'stop',
    toolName: string
  ) => {
    // Define write tools that should switch mode
    const WRITE_TOOLS = new Set(['write_file', 'patch_file', 'multi_edit']);

    // Send approval response via WebSocket (for stream mode)
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(
        JSON.stringify({
          type: 'approval_response',
          approval_id: approvalId,
          response: response,
        })
      );
    } else if (agentExecuting) {
      // Send approval response via HTTP API (for SSE agent mode)
      try {
        await chatApi.sendApprovalResponse(approvalId, response);
        console.log('[APPROVAL] Response sent via HTTP API');
      } catch (error) {
        console.error('[APPROVAL] Failed to send response:', error);
        toast.error('Failed to send approval response');
        return;
      }
    }

    // Remove approval message from chat
    setMessages((prev) =>
      prev.filter((msg) => !(msg.type === 'approval_request' && msg.approvalId === approvalId))
    );

    // Handle mode switching for write tools
    if (response === 'allow_all' && WRITE_TOOLS.has(toolName)) {
      // Switch to "Allow All Edits" mode
      setEditMode('allow');
      toast.success('Switched to "Allow All Edits" mode');
    } else if (response === 'allow_once') {
      toast.success('Approved this operation');
    } else if (response === 'allow_all') {
      toast.success('Approved all operations of this type for this session');
    } else {
      toast.error('Operation cancelled');
    }
  };

  const renderMessageContent = (content: string, isCurrentlyStreaming: boolean = false) => {
    // Safety check: handle undefined/null content
    if (!content) {
      return <span className="text-gray-400 italic">No content available</span>;
    }

    let processedContent = content;

    if (isCurrentlyStreaming) {
      processedContent = processedContent.replace(
        /```\w+\s*\n\/\/\s*File:\s*([^\n]+)[\s\S]*?```/g,
        (_match, fileName) => {
          return `[FILE: ${fileName.trim()}]`;
        }
      );
      processedContent = processedContent.replace(
        /```\w+\s*\n\/\/\s*File:\s*([^\n]+)[\s\S]*$/g,
        (_match, fileName) => {
          return `[FILE: ${fileName.trim()}]`;
        }
      );
    } else {
      processedContent = processedContent.replace(/```[\s\S]*?```/g, (match) => {
        const fileMatch = match.match(/```\w+\s*\n\/\/\s*File:\s*([^\n]+)/);
        if (fileMatch) {
          return `[FILE: ${fileMatch[1].trim()}]`;
        }
        return '';
      });
    }

    const parts = processedContent.split(/\[FILE: ([^\]]+)\]/g);

    return parts.map((part, index) => {
      if (index % 2 === 0) {
        return <span key={index}>{part}</span>;
      } else {
        const fileName = part;
        const fileInfo = streamingFiles.get(fileName);
        const isFileStreaming =
          isCurrentlyStreaming && (!fileInfo || fileInfo.isStreaming !== false);

        return (
          <div key={index} className="my-2">
            <div className="flex items-center gap-2 p-3 bg-[var(--surface)]/50 rounded-lg border border-[var(--border-color)]">
              <FileCode size={18} className="text-[var(--primary)]" />
              <span className="text-sm font-medium flex-1">{fileName}</span>
              {isFileStreaming && (
                <Loader2 className="animate-spin text-[var(--primary)]" size={16} />
              )}
              {!isFileStreaming && (
                <div className="w-4 h-4 rounded-full bg-green-500 flex items-center justify-center">
                  <div className="w-2 h-2 bg-white rounded-full" />
                </div>
              )}
            </div>
          </div>
        );
      }
    });
  };

  // Memoize the style object to avoid recreating it on every render
  const containerStyle = useMemo(() => {
    if (isDocked) return undefined;
    if (isDesktop) {
      return {
        left: sidebarExpanded ? 'calc(96px + 50vw)' : 'calc(24px + 50vw)',
        transition:
          'left 0.4s cubic-bezier(0.34, 1.56, 0.64, 1), width 0.4s var(--ease), max-height 0.4s var(--ease)',
      };
    }
    return {
      transition: 'width 0.4s var(--ease), max-height 0.4s var(--ease), transform 0.4s var(--ease)',
    };
  }, [isDocked, isDesktop, sidebarExpanded]);

  return (
    <>
      {/* Mobile: Floating chat button - only show when collapsed and not docked */}
      {!isDocked && (
        <div className="md:hidden fixed bottom-20 right-4 z-30 group">
          <button
            onClick={() => setIsExpanded(true)}
            className={`
            w-12 h-12 md:w-16 md:h-16 rounded-full
            bg-[var(--primary)] hover:bg-[var(--primary-hover)] active:bg-[var(--primary-hover)]
            shadow-lg hover:shadow-xl
            flex items-center justify-center
            transition-all duration-300
            hover:scale-110
            ${isExpanded ? 'opacity-0 pointer-events-none scale-0' : 'opacity-100 scale-100'}
          `}
            aria-label="Open agents panel"
          >
            <svg
              xmlns="http://www.w3.org/2000/svg"
              width="20"
              height="16"
              viewBox="0 0 161.9 126.66"
              className="text-white md:w-6 md:h-6"
              fill="currentColor"
            >
              <path d="m13.45,46.48h54.06c10.21,0,16.68-10.94,11.77-19.89l-9.19-16.75c-2.36-4.3-6.87-6.97-11.77-6.97H22.41c-4.95,0-9.5,2.73-11.84,7.09L1.61,26.71c-4.79,8.95,1.69,19.77,11.84,19.77Z" />
              <path d="m61.05,119.93l26.95-46.86c5.09-8.85-1.17-19.91-11.37-20.12l-19.11-.38c-4.9-.1-9.47,2.48-11.91,6.73l-17.89,31.12c-2.47,4.29-2.37,9.6.25,13.8l10.05,16.13c5.37,8.61,17.98,8.39,23.04-.41Z" />
              <path d="m148.46,0h-54.06c-10.21,0-16.68,10.94-11.77,19.89l9.19,16.75c2.36,4.3,6.87,6.97,11.77,6.97h35.9c4.95,0,9.5-2.73,11.84-7.09l8.97-16.75C165.08,10.82,158.6,0,148.46,0Z" />
            </svg>

            {/* Hover tooltip */}
            <div
              className="
            absolute bottom-full mb-2 right-0
            bg-gray-900 text-white text-sm
            px-3 py-2 rounded-lg
            whitespace-nowrap
            opacity-0 group-hover:opacity-100
            transition-opacity duration-200
            pointer-events-none
          "
            >
              Open chat
            </div>
          </button>
        </div>
      )}

      {/* Chat container */}
      {/* When docked: fills parent with rounded corners */}
      {/* When floating: fixed position, centered, same design language */}
      <div
        ref={containerRef}
        className={`
          chat-container
          flex flex-col min-h-0
          bg-[var(--bg)]
          ${
            isDocked
              ? 'w-full h-full rounded-[var(--radius)] border border-[var(--border-hover)]'
              : `
              fixed
              z-40
              border border-[var(--border-hover)]
              transition-all duration-400 ease-[var(--ease)]
              rounded-[var(--radius)]
              max-md:bottom-0 max-md:left-0 max-md:right-0 max-md:rounded-b-none max-md:w-full
              md:bottom-6 md:-translate-x-1/2
              ${
                isExpanded
                  ? 'md:w-[min(800px,calc(100vw-48px))] md:max-h-[calc(100vh-48px)] max-md:max-h-[90vh] max-md:translate-y-0'
                  : 'md:w-[min(650px,calc(100vw-48px))] max-md:translate-y-full max-md:opacity-0 max-md:pointer-events-none'
              }
            `
          }
          ${className}
        `}
        style={containerStyle}
      >
        {/* Mobile header with close button - only shown when floating (not docked) */}
        {!isDocked && (
          <div className="md:hidden flex items-center justify-between px-4 py-3 border-b border-[var(--border)]">
            <h3 className="text-xs font-semibold text-[var(--text)]">Agents</h3>
            <button
              onClick={() => setIsExpanded(false)}
              className="btn btn-icon btn-sm"
              aria-label="Close agents panel"
            >
              <X size={16} />
            </button>
          </div>
        )}

        {/* Session header bar */}
        {effectiveIsExpanded && (
          <div className="relative flex items-center gap-2 px-3 py-2 border-b border-[var(--border)]">
            <button
              ref={sessionsButtonRef}
              onClick={() => setShowSessionPopover((v) => !v)}
              className="relative flex items-center gap-1.5 rounded-full px-2 py-1 text-[var(--text-muted)] hover:text-[var(--text)] hover:bg-[var(--surface-hover)] transition-colors"
              aria-label="Agent sessions"
            >
              <List size={16} />
              {sessions.length > 1 && (
                <span className="text-[10px] font-semibold text-[var(--text-subtle)]">
                  {sessions.length}
                </span>
              )}
            </button>

            {isRenamingTitle ? (
              <input
                type="text"
                value={renameTitleValue}
                onChange={(e) => setRenameTitleValue(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    // Blur the input — onBlur handles the commit
                    e.currentTarget.blur();
                  } else if (e.key === 'Escape') {
                    e.preventDefault();
                    // Cancel: clear value so onBlur skips the API call
                    setRenameTitleValue('');
                    e.currentTarget.blur();
                  }
                }}
                onBlur={() => {
                  if (currentChatId && renameTitleValue.trim()) {
                    handleRenameSession(currentChatId, renameTitleValue.trim());
                  }
                  setIsRenamingTitle(false);
                }}
                className="flex-1 min-w-0 border-b-2 border-[var(--primary)] bg-transparent text-sm text-[var(--text)] outline-none"
                autoFocus
              />
            ) : (
              <span
                onClick={() => {
                  setRenameTitleValue(currentSessionTitle);
                  setIsRenamingTitle(true);
                }}
                className="flex-1 truncate text-xs text-[var(--text-muted)] hover:text-[var(--text)] transition-colors cursor-text"
              >
                {currentSessionTitle}
              </span>
            )}

            {activeMcpServers.length > 0 && (
              <div className="flex items-center gap-1 mx-1">
                {activeMcpServers.slice(0, 3).map((s) => (
                  <span
                    key={s.slug}
                    className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-[10px] font-medium bg-purple-500/10 text-purple-400 border border-purple-500/20"
                    title={s.name}
                  >
                    <Plug size={10} />
                    {s.slug}
                  </span>
                ))}
                {activeMcpServers.length > 3 && (
                  <span className="text-[10px] text-[var(--text-muted)]">
                    +{activeMcpServers.length - 3}
                  </span>
                )}
              </div>
            )}

            <button
              onClick={handleNewSession}
              className="btn btn-icon btn-sm"
              aria-label="New agent session"
            >
              <Plus size={14} />
            </button>

            <ChatSessionPopover
              isOpen={showSessionPopover}
              onClose={() => setShowSessionPopover(false)}
              sessions={sessions}
              currentSessionId={currentChatId}
              onSelectSession={handleSelectSession}
              onNewSession={handleNewSession}
              onRenameSession={handleRenameSession}
              onDeleteSession={handleDeleteSession}
              sessionCount={sessions.length}
              anchorRef={sessionsButtonRef}
            />
          </div>
        )}

        {/* Chat messages - always visible when docked, otherwise only when expanded */}
        <div
          ref={messagesContainerRef}
          className={`
          chat-messages
          flex-1 min-h-0 overflow-y-auto px-3
          transition-all duration-300
          ${effectiveIsExpanded ? 'pointer-events-auto' : 'pointer-events-none'}
          ${
            effectiveIsExpanded
              ? `opacity-100 py-3 ${isDocked ? '' : 'max-h-[calc(100vh-400px)]'}`
              : 'opacity-0 max-h-0 py-0'
          }
        `}
        >
          {isLoadingHistory && (
            <div className="text-center text-[var(--text-muted)] mt-8 space-y-4">
              <div className="w-14 h-14 bg-[var(--surface)] rounded-[var(--radius)] flex items-center justify-center mx-auto border border-[var(--border)]">
                <Loader2 className="animate-spin text-[var(--text-muted)]" size={28} />
              </div>
              <div className="space-y-2">
                <p className="text-sm max-w-xs mx-auto leading-relaxed">Loading chat history...</p>
              </div>
            </div>
          )}

          {!isLoadingHistory && messages.length === 0 && !isStreaming && (
            <div className="text-center text-[var(--text-muted)] mt-8 space-y-6 max-w-md mx-auto px-4">
              <div className="w-14 h-14 bg-[var(--surface)] rounded-[var(--radius)] flex items-center justify-center mx-auto border border-[var(--border)]">
                <svg
                  xmlns="http://www.w3.org/2000/svg"
                  width="28"
                  height="22"
                  viewBox="0 0 161.9 126.66"
                  className="text-[var(--text-muted)]"
                >
                  <g>
                    <path
                      d="m13.45,46.48h54.06c10.21,0,16.68-10.94,11.77-19.89l-9.19-16.75c-2.36-4.3-6.87-6.97-11.77-6.97H22.41c-4.95,0-9.5,2.73-11.84,7.09L1.61,26.71c-4.79,8.95,1.69,19.77,11.84,19.77Z"
                      fill="currentColor"
                      strokeWidth="0"
                    />
                    <path
                      d="m61.05,119.93l26.95-46.86c5.09-8.85-1.17-19.91-11.37-20.12l-19.11-.38c-4.9-.1-9.47,2.48-11.91,6.73l-17.89,31.12c-2.47,4.29-2.37,9.6.25,13.8l10.05,16.13c5.37,8.61,17.98,8.39,23.04-.41Z"
                      fill="currentColor"
                      strokeWidth="0"
                    />
                    <path
                      d="m148.46,0h-54.06c-10.21,0-16.68,10.94-11.77,19.89l9.19,16.75c2.36,4.3,6.87,6.97,11.77,6.97h35.9c4.95,0,9.5-2.73,11.84-7.09l8.97-16.75C165.08,10.82,158.6,0,148.46,0Z"
                      fill="currentColor"
                      strokeWidth="0"
                    />
                  </g>
                </svg>
              </div>
              <div className="space-y-2">
                <p className="text-sm font-semibold text-[var(--text)]">Let's start building</p>
                <p className="text-xs leading-relaxed text-[var(--text-muted)]">
                  Describe what you'd like to create and I'll help you build it step by step
                </p>
              </div>

              {/* Discovery Cards */}
              <div className="space-y-2 text-left">
                <div className="bg-[var(--surface)] rounded-[var(--radius-medium)] p-3 border border-[var(--border)]">
                  <div className="flex items-center gap-2 mb-2">
                    <PencilSimple size={14} weight="bold" className="text-[var(--text-muted)]" />
                    <span className="font-medium text-xs text-[var(--text)]">
                      Customize Your Agent
                    </span>
                  </div>
                  <p className="text-xs text-[var(--text-muted)] mb-3">
                    Edit system prompts, behaviors, and settings to tailor {currentAgent.name} to
                    your needs.
                  </p>
                  <button
                    onClick={() => {
                      navigate('/library', { state: { selectedAgentId: currentAgent.backendId } });
                    }}
                    className="btn btn-tab w-full"
                  >
                    Open in Library
                  </button>
                </div>

                <div className="bg-[var(--surface)] rounded-[var(--radius-medium)] p-3 border border-[var(--border)]">
                  <div className="flex items-center gap-2 mb-2">
                    <Storefront size={14} weight="fill" className="text-[var(--text-muted)]" />
                    <span className="font-medium text-xs text-[var(--text)]">
                      Discover More Agents
                    </span>
                  </div>
                  <p className="text-xs text-[var(--text-muted)] mb-3">
                    Browse specialized agents for React, Vue, Python, DevOps, and more!
                  </p>
                  <button
                    onClick={() => {
                      navigate('/marketplace');
                    }}
                    className="btn btn-primary w-full"
                  >
                    Browse Marketplace
                  </button>
                </div>
              </div>
            </div>
          )}

          {messages.map((message) => {
            // Check if this is a new message that should animate
            const isNewMessage = !animatedMessagesRef.current.has(message.id);
            if (isNewMessage && !isLoadingHistory) {
              animatedMessagesRef.current.add(message.id);
            }
            const shouldAnimate = isNewMessage && !isLoadingHistory;

            // Render approval request message
            if (message.type === 'approval_request' && message.approvalId) {
              return (
                <div
                  key={message.id}
                  className={`mb-4 ${shouldAnimate ? 'animate-[slideIn_0.2s_ease-out]' : ''}`}
                >
                  <ApprovalRequestCard
                    approvalId={message.approvalId}
                    toolName={message.toolName || 'unknown'}
                    toolParameters={message.toolParameters ?? {}}
                    toolDescription={message.toolDescription || 'No description provided'}
                    onRespond={handleApprovalResponse}
                  />
                </div>
              );
            }

            // Render workspace-attach prompt (request_workspace tool paused)
            if (message.type === 'workspace_attach_request' && message.workspaceAttachInputId) {
              return (
                <div
                  key={message.id}
                  className={`mb-4 ${shouldAnimate ? 'animate-[slideIn_0.2s_ease-out]' : ''}`}
                >
                  <WorkspaceAttachCard
                    mode="agent-prompt"
                    inputId={message.workspaceAttachInputId}
                    candidates={message.workspaceAttachCandidates || []}
                    reason={message.workspaceAttachReason}
                  />
                </div>
              );
            }

            // Render agent message with special component
            if (message.type === 'ai' && message.agentData) {
              return (
                <div
                  key={message.id}
                  className={`mb-4 ${shouldAnimate ? 'animate-[slideIn_0.2s_ease-out]' : ''}`}
                >
                  <AgentMessage
                    agentData={message.agentData}
                    finalResponse={message.content}
                    agentIcon={message.agentIcon}
                    agentAvatarUrl={message.agentAvatarUrl}
                    toolCallsCollapsed={toolCallsCollapsed}
                  />
                </div>
              );
            }

            // Render regular messages
            const isLastUser = message.id === lastUserMessageId;
            return (
              <div
                key={message.id}
                className={shouldAnimate ? 'animate-[slideIn_0.2s_ease-out]' : ''}
              >
                <ChatMessage
                  type={message.type as 'user' | 'ai'}
                  content={message.content || ''}
                  agentIcon={message.agentIcon}
                  agentAvatarUrl={message.agentAvatarUrl}
                  attachments={message.attachments}
                  toolCalls={message.toolCalls}
                  actions={message.actions}
                  timestamp={message.timestamp}
                  showRetry={isLastUser && !agentExecuting && !isStreaming}
                  onRetry={isLastUser ? () => handleRetryMessage(message.content) : undefined}
                />
              </div>
            );
          })}

          {/* Streaming message */}
          {isStreaming && currentStream && (
            <div className="mb-4 animate-[slideIn_0.3s_ease-out]">
              <ChatMessage
                type="ai"
                content={renderMessageContent(currentStream, true)}
                agentAvatarUrl={currentAgent?.avatar_url}
              />
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>

        {/* Pause banner — pinned just above the input so the user can't miss
            that the agent stopped and which card needs filling. */}
        {pendingPauses.length > 0 && (
          <div className="pointer-events-auto px-3 pb-2 space-y-2">
            {pendingPauses.map((p) => (
              <button
                key={p.inputId}
                type="button"
                onClick={onOpenConfigTab}
                disabled={!onOpenConfigTab}
                className="w-full flex items-center gap-2 px-3 py-2 bg-[var(--primary)]/10 border border-[var(--primary)]/40 rounded-[var(--radius-small)] text-left hover:bg-[var(--primary)]/15 transition-colors disabled:cursor-default"
              >
                <Pause size={14} className="text-[var(--primary)] flex-shrink-0" />
                <div className="min-w-0 flex-1">
                  <p className="text-[11px] font-medium text-[var(--text)] truncate">
                    Agent paused — waiting on{' '}
                    <span className="font-semibold">{p.containerName}</span> config
                  </p>
                  <p className="text-[10px] text-[var(--text-muted)] truncate">
                    {onOpenConfigTab
                      ? 'Click to open the Config tab and fill the card.'
                      : 'Open the Config tab to fill the card.'}
                  </p>
                </div>
              </button>
            ))}
          </div>
        )}

        {/* Chat input */}
        <div onFocus={handleInputFocus} className="pointer-events-auto">
          <ChatInput
            agents={agents}
            currentAgent={currentAgent}
            onSelectAgent={handleAgentSelect}
            onModelChange={handleModelChange}
            onSendMessage={handleSendMessage}
            slug={projectSlug}
            projectName={projectName}
            disabled={isStreaming || agentExecuting || disabledProp}
            viewerMode={!!disabledProp}
            isExecuting={agentExecuting}
            onStop={stopAgentExecution}
            onClearHistory={handleClearHistory}
            onUndo={handleUndo}
            onRetry={handleRetry}
            editMode={editMode}
            onModeChange={setEditMode}
            onPlanMode={() => setEditMode('plan')}
            isDocked={isDocked}
            prefillMessage={prefillMessage}
            prefillMentions={prefillMentions}
            onPrefillConsumed={onPrefillConsumed}
            toolCallsCollapsed={toolCallsCollapsed}
            onToggleToolCallsCollapsed={() => setToolCallsCollapsed((prev) => !prev)}
            availableSkills={availableSkills}
            isAdmin={isAdmin}
            onOpenDebugTools={() => setShowDebugModal(true)}
            currentModelSupportsVision={currentModelSupportsVision}
            onCompact={handleCompact}
          />
        </div>
      </div>

      {/* Admin-only debug modal */}
      {isAdmin && (
        <ToolDebugModal
          isOpen={showDebugModal}
          onClose={() => setShowDebugModal(false)}
          projectId={projectId}
        />
      )}
    </>
  );
}
