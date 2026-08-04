import { EventBus } from '../core/EventBus';
import { Events, ChatMessage } from '../types';

const STORAGE_KEY = 'zancord_chat';
const MAX_MESSAGES = 500;

export class ChatManager {
  private eventBus: EventBus;
  private container: HTMLElement;
  private form: HTMLFormElement;
  private input: HTMLInputElement;
  private unreadBadge: HTMLElement | null;
  private isDrawerOpen: boolean = false;
  private unreadCount: number = 0;
  private messages: ChatMessage[] = [];

  constructor(
    eventBus: EventBus,
    container: HTMLElement,
    form: HTMLFormElement,
    unreadBadge: HTMLElement | null = null
  ) {
    this.eventBus = eventBus;
    this.container = container;
    this.form = form;
    this.input = form.querySelector('input') as HTMLInputElement;
    this.unreadBadge = unreadBadge;

    this.loadMessages();
    this.setupListeners();
  }

  private escapeHTML(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }

  private formatTime(timestamp: number): string {
    const date = new Date(timestamp);
    const hours = date.getHours().toString().padStart(2, '0');
    const minutes = date.getMinutes().toString().padStart(2, '0');
    return `${hours}:${minutes}`;
  }

  private generateId(): string {
    return Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  }

  private setupListeners(): void {
    this.form.addEventListener('submit', (e) => {
      e.preventDefault();
      const text = this.input.value.trim();
      if (text) {
        this.eventBus.emit(Events.CHAT_MESSAGE_SENT, { text });
        this.input.value = '';
      }
    });

    this.eventBus.on<ChatMessage>(Events.CHAT_MESSAGE_RECEIVED, (msg) => {
      // Assign an id if the message doesn't have one (server doesn't send ids)
      if (!msg.id) msg.id = this.generateId();
      this.addMessage(msg);
    });

    // Remote peer deleted a message
    this.eventBus.on<{ id: string }>(Events.CHAT_MESSAGE_DELETED, ({ id }) => {
      this.removeMessageLocally(id);
    });

    // Remote peer cleared all messages
    this.eventBus.on(Events.CHAT_CLEARED, () => {
      this.clearMessagesLocally();
    });

    // Clear all button
    const btnClear = document.getElementById('btn-clear-chat');
    btnClear?.addEventListener('click', () => this.clearMessages());
  }

  public addMessage(msg: ChatMessage): void {
    // Ensure message has an id
    if (!msg.id) msg.id = this.generateId();

    // Store in memory and persist
    this.messages.push(msg);
    // Prune if over limit
    if (this.messages.length > MAX_MESSAGES) {
      this.messages = this.messages.slice(-MAX_MESSAGES);
    }
    this.saveMessages();

    // Render the message DOM element
    this.renderMessage(msg);

    this.container.scrollTop = this.container.scrollHeight;

    if (!this.isDrawerOpen && !msg.isLocal) {
      this.unreadCount++;
      this.updateUnreadBadge();
    }
  }

  private renderMessage(msg: ChatMessage): void {
    const msgEl = document.createElement('div');
    msgEl.className = `chat-message ${msg.isLocal ? 'local' : 'remote'}`;
    msgEl.dataset.msgId = msg.id;

    const safeSender = this.escapeHTML(msg.sender);
    const safeText = this.escapeHTML(msg.text);
    const timeStr = this.formatTime(msg.timestamp);

    msgEl.innerHTML = `
      <span class="chat-sender">${safeSender}</span>
      <p class="chat-text">${safeText}</p>
      <span class="chat-time">${timeStr}</span>
      <button class="chat-delete-btn" title="Delete message"><i class="fa-solid fa-xmark"></i></button>
    `;

    // Wire up delete button
    const deleteBtn = msgEl.querySelector('.chat-delete-btn') as HTMLButtonElement;
    deleteBtn?.addEventListener('click', (e) => {
      e.stopPropagation();
      this.deleteMessage(msg.id);
    });

    this.container.appendChild(msgEl);
  }

  public deleteMessage(id: string): void {
    this.removeMessageLocally(id);
    // Notify the other peer
    this.eventBus.emit(Events.CHAT_MESSAGE_DELETED, { id });
  }

  /** Remove from storage + DOM without emitting (used for remote deletes) */
  private removeMessageLocally(id: string): void {
    this.messages = this.messages.filter((m) => m.id !== id);
    this.saveMessages();

    const el = this.container.querySelector(`[data-msg-id="${id}"]`);
    if (el) {
      el.classList.add('chat-message-removing');
      el.addEventListener('animationend', () => el.remove(), { once: true });
    }
  }

  public setDrawerOpen(open: boolean): void {
    this.isDrawerOpen = open;
    if (open) {
      this.unreadCount = 0;
      this.updateUnreadBadge();
    }
  }

  private updateUnreadBadge(): void {
    if (!this.unreadBadge) return;
    if (this.unreadCount > 0) {
      this.unreadBadge.textContent = this.unreadCount.toString();
      this.unreadBadge.classList.remove('hidden');
    } else {
      this.unreadBadge.classList.add('hidden');
    }
  }

  public clearMessages(): void {
    this.clearMessagesLocally();
    // Notify the other peer
    this.eventBus.emit(Events.CHAT_CLEARED, {});
  }

  /** Clear storage + DOM without emitting (used for remote clears) */
  private clearMessagesLocally(): void {
    this.messages = [];
    this.saveMessages();
    this.container.innerHTML = `
      <div class="chat-system-msg">Chat cleared.</div>
    `;
    this.unreadCount = 0;
    this.updateUnreadBadge();
  }

  private saveMessages(): void {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify(this.messages));
    } catch {
      // Storage full — prune older half and retry
      this.messages = this.messages.slice(Math.floor(this.messages.length / 2));
      try {
        localStorage.setItem(STORAGE_KEY, JSON.stringify(this.messages));
      } catch { /* give up silently */ }
    }
  }

  private loadMessages(): void {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (!raw) return;
      const parsed = JSON.parse(raw) as ChatMessage[];
      if (!Array.isArray(parsed)) return;

      this.messages = parsed.slice(-MAX_MESSAGES);

      // Clear the default system message
      this.container.innerHTML = '';

      // Add the system notice at the top
      const notice = document.createElement('div');
      notice.className = 'chat-system-msg';
      notice.textContent = 'Chat history restored. Messages are stored locally on your device.';
      this.container.appendChild(notice);

      // Render all loaded messages
      for (const msg of this.messages) {
        this.renderMessage(msg);
      }

      // Scroll to bottom
      this.container.scrollTop = this.container.scrollHeight;
    } catch {
      // Corrupt data — start fresh
      this.messages = [];
    }
  }
}
