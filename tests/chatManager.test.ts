// @vitest-environment happy-dom
import { describe, it, expect, beforeEach } from 'vitest';
import { ChatManager } from '../src/ui/ChatManager';
import { EventBus } from '../src/core/EventBus';
import { Events, ChatMessage } from '../src/types';

function makeChat(): {
  chat: ChatManager;
  bus: EventBus;
  container: HTMLElement;
  input: HTMLInputElement;
  badge: HTMLElement;
} {
  document.body.innerHTML = '';
  const bus = new EventBus();
  const container = document.createElement('div');
  container.id = 'chat-messages';
  document.body.appendChild(container);
  const form = document.createElement('form');
  form.innerHTML = '<input type="text">';
  document.body.appendChild(form);
  const badge = document.createElement('span');
  badge.className = 'unread-badge hidden';
  document.body.appendChild(badge);
  const input = form.querySelector('input') as HTMLInputElement;
  const chat = new ChatManager(bus, container, form, badge);
  return { chat, bus, container, input, badge };
}

describe('ChatManager (U1)', () => {
  it('emits CHAT_MESSAGE_SENT on form submit', () => {
    const { chat, bus, input } = makeChat();
    let sent: string | null = null;
    bus.on(Events.CHAT_MESSAGE_SENT, ({ text }) => (sent = text));
    input.value = '  hello  ';
    const form = input.closest('form')!;
    form.dispatchEvent(new Event('submit', { cancelable: true }));
    expect(sent).toBe('hello');
    expect(input.value).toBe('');
  });

  it('renders received messages and bumps the unread badge', () => {
    const { bus, container, badge } = makeChat();
    const msg: ChatMessage = { peerId: 'p1', sender: 'Alice', text: 'hi', timestamp: Date.now(), isLocal: false };
    bus.emit(Events.CHAT_MESSAGE_RECEIVED, msg);
    expect(container.querySelector('.chat-message')).toBeTruthy();
    expect(container.textContent).toContain('Alice');
    expect(container.textContent).toContain('hi');
    expect(badge.classList.contains('hidden')).toBe(false);
    expect(badge.textContent).toBe('1');
  });

  it('does not bump the unread badge for local messages or when open', () => {
    const { bus, badge } = makeChat();
    const base: ChatMessage = { peerId: 'me', sender: 'Me', text: 'x', timestamp: Date.now(), isLocal: false };
    bus.emit(Events.CHAT_MESSAGE_RECEIVED, { ...base, peerId: 'me', isLocal: true });
    expect(badge.classList.contains('hidden')).toBe(true);
  });

  it('clears unread when the drawer opens', () => {
    const { chat, bus, badge } = makeChat();
    const msg: ChatMessage = { peerId: 'p1', sender: 'A', text: 'x', timestamp: Date.now(), isLocal: false };
    bus.emit(Events.CHAT_MESSAGE_RECEIVED, msg);
    expect(badge.textContent).toBe('1');
    chat.setDrawerOpen(true);
    expect(badge.classList.contains('hidden')).toBe(true);
  });

  it('escapes HTML in sender and text', () => {
    const { bus, container } = makeChat();
    const msg: ChatMessage = {
      peerId: 'p1',
      sender: '<b>Bob</b>',
      text: '<script>alert(1)</script>',
      timestamp: Date.now(),
      isLocal: false,
    };
    bus.emit(Events.CHAT_MESSAGE_RECEIVED, msg);
    expect(container.querySelector('script')).toBeNull();
    expect(container.querySelector('b')).toBeNull();
    expect(container.textContent).toContain('<b>Bob</b>');
    expect(container.textContent).toContain('<script>alert(1)</script>');
  });
});
