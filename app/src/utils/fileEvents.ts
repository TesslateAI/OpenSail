// Custom event system for file changes
// This allows components to communicate file changes without polling the server

type FileEventType =
  | 'file-created'
  | 'file-updated'
  | 'file-deleted'
  | 'files-changed'
  | 'files-written';

interface FileEventDetail {
  type: FileEventType;
  filePath?: string;
  timestamp: number;
}

class FileEventBus {
  private eventTarget: EventTarget;

  constructor() {
    this.eventTarget = new EventTarget();
  }

  // Emit a file change event
  emit(type: FileEventType, filePath?: string) {
    const event = new CustomEvent<FileEventDetail>('file-change', {
      detail: {
        type,
        filePath,
        timestamp: Date.now()
      }
    });
    this.eventTarget.dispatchEvent(event);
  }

  // Listen for file change events
  on(callback: (detail: FileEventDetail) => void) {
    const handler = (event: Event) => {
      const customEvent = event as CustomEvent<FileEventDetail>;
      callback(customEvent.detail);
    };
    this.eventTarget.addEventListener('file-change', handler);
    return () => this.eventTarget.removeEventListener('file-change', handler);
  }
}

// Export a singleton instance
export const fileEvents = new FileEventBus();
