import { EventBus } from '../core/EventBus';
import { Events, ToastOptions } from '../types';

export class ToastManager {
  private container: HTMLElement;
  private eventBus: EventBus;

  constructor(container: HTMLElement, eventBus: EventBus) {
    this.container = container;
    this.eventBus = eventBus;

    this.setupListeners();
  }

  private setupListeners(): void {
    this.eventBus.on<ToastOptions>(Events.TOAST, (options) => {
      this.showToast(options);
    });
  }

  public showToast(options: ToastOptions): void {
    const { message, type = 'info', duration = 3000 } = options;

    const toast = document.createElement('div');
    toast.className = `toast toast-${type}`;

    const iconMap: Record<string, string> = {
      info: 'fa-circle-info',
      success: 'fa-circle-check',
      warning: 'fa-triangle-exclamation',
      error: 'fa-circle-xmark',
    };

    const icon = iconMap[type] || 'fa-circle-info';

    toast.innerHTML = `
      <i class="fa-solid ${icon}"></i>
      <span class="toast-message">${this.escapeHTML(message)}</span>
    `;

    // Prepend so newest toast appears on top
    this.container.prepend(toast);

    // Auto dismiss after duration
    setTimeout(() => {
      toast.classList.add('dismissing');
      setTimeout(() => {
        if (toast.parentNode) {
          toast.parentNode.removeChild(toast);
        }
      }, 300);
    }, duration);
  }

  private escapeHTML(str: string): string {
    return str
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#039;');
  }
}
