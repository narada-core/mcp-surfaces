import { createHash, randomUUID } from 'node:crypto';
import {
  assertAllowedOrigin,
  boundedDelay,
  BrowserControlError,
  CdpConnection,
  type CdpTarget,
  listCdpTargets,
  normalizeAllowedOrigins,
  validateCdpEndpoint,
} from './cdp.js';
import { boundedCollection } from '@narada-core/mcp-transport/bounded-collection';

const MAX_SELECTOR_LENGTH = 512;
const MAX_TEXT_LENGTH = 4_000;
const MAX_SNAPSHOT_NODES = 500;
const MAX_WAIT_MS = 15_000;
const SENSITIVE_PATTERN = /password|passcode|token|secret|api[-_ ]?key|cookie|authorization|credential|private[-_ ]?key|client[-_ ]?secret/i;

export { BrowserControlError } from './cdp.js';

export type BrowserActionIntent = 'verify' | 'login' | 'submit' | 'destructive';

export type BrowserSessionInfo = {
  profile_id: string;
  session_id: string;
  cdp_endpoint: string;
  allowed_origins: string[];
  target: { id: string; type: string; title: string; url: string };
  attached_at: string;
  last_action: string | null;
};

type NodeDescription = {
  nodeId?: number;
  nodeName?: string;
  attributes?: string[];
  isContentEditable?: boolean;
};

function requireText(value: unknown, field: string, maxLength = MAX_TEXT_LENGTH): string {
  if (typeof value !== 'string' || value.trim().length === 0) {
    throw new BrowserControlError('argument_required', `${field} is required.`);
  }
  if (value.length > maxLength) {
    throw new BrowserControlError('argument_too_long', `${field} exceeds its bounded length.`, { field, max_length: maxLength });
  }
  return value;
}

function selector(value: unknown): string {
  return requireText(value, 'selector', MAX_SELECTOR_LENGTH);
}

function int(value: unknown, fallback: number, min: number, max: number, field: string): number {
  if (value === undefined || value === null) return fallback;
  if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) {
    throw new BrowserControlError('argument_out_of_range', `${field} must be an integer from ${min} to ${max}.`, { field, min, max });
  }
  return value;
}

function attributesRecord(description: NodeDescription): Record<string, string> {
  const values: Record<string, string> = {};
  const attributes = description.attributes ?? [];
  for (let index = 0; index + 1 < attributes.length; index += 2) values[attributes[index].toLowerCase()] = attributes[index + 1];
  return values;
}

export function isSensitiveField(selectorValue: string, description: NodeDescription): boolean {
  const attrs = attributesRecord(description);
  const searchable = [
    selectorValue,
    description.nodeName ?? '',
    attrs.id ?? '',
    attrs.name ?? '',
    attrs.type ?? '',
    attrs.autocomplete ?? '',
    attrs['aria-label'] ?? '',
    attrs.placeholder ?? '',
  ].join(' ');
  return (attrs.type ?? '').toLowerCase() === 'password' || SENSITIVE_PATTERN.test(searchable);
}

export function requireConfirmedIntent(intent: BrowserActionIntent, confirmed: unknown): void {
  if (intent !== 'verify' && confirmed !== true) {
    throw new BrowserControlError('confirmation_required', `confirmed:true is required for ${intent} intent.`, {
      intent,
      required: 'confirmed:true',
    });
  }
}

function safeText(value: unknown, maxLength = 600): string {
  if (typeof value !== 'string') return '';
  const text = value.replace(/[\u0000-\u001f\u007f]/g, ' ').trim();
  return text.length > maxLength ? `${text.slice(0, maxLength)}…` : text;
}

function valueOf(value: unknown): string {
  if (value && typeof value === 'object' && 'value' in value) return safeText((value as any).value);
  return safeText(value);
}

function safeUrl(value: unknown, maxLength = 2_000): string {
  if (typeof value !== 'string') return '';
  try {
    const parsed = new URL(value);
    parsed.username = '';
    parsed.password = '';
    for (const [key] of parsed.searchParams) {
      if (SENSITIVE_PATTERN.test(key)) parsed.searchParams.set(key, '[redacted]');
    }
    if (parsed.hash) parsed.hash = '#[redacted]';
    return safeText(parsed.toString(), maxLength);
  } catch {
    return safeText(value, maxLength);
  }
}

function redactMarkup(value: string): string {
  return value
    .replace(/(\b(?:value|data-token|data-secret|data-api-key|authorization|cookie|csrf-token|access-token)=["'])[^"']*(["'])/gi, '$1[redacted]$2')
    .replace(/(\b(?:value|data-token|data-secret|data-api-key|authorization|cookie|csrf-token|access-token)=[''])[^']*([''])/gi, '$1[redacted]$2')
    .replace(/<script\b[^>]*>[\s\S]*?<\/script>/gi, '<script>[redacted]</script>')
    .replace(/<style\b[^>]*>[\s\S]*?<\/style>/gi, '<style>[redacted]</style>');
}

function quadCenter(quad: number[]): { x: number; y: number } {
  if (!Array.isArray(quad) || quad.length < 8) throw new BrowserControlError('element_box_missing', 'The selected element has no usable layout box.');
  return {
    x: (quad[0] + quad[2] + quad[4] + quad[6]) / 4,
    y: (quad[1] + quad[3] + quad[5] + quad[7]) / 4,
  };
}

export class BrowserSession {
  readonly profileId: string;
  readonly sessionId: string;
  readonly cdpEndpoint: string;
  readonly allowedOrigins: string[];
  readonly attachedAt: string;
  private target: CdpTarget;
  private readonly connection: CdpConnection;
  private lastAction: string | null = null;

  private constructor(
    profileId: string,
    sessionId: string,
    cdpEndpoint: string,
    allowedOrigins: string[],
    target: CdpTarget,
    connection: CdpConnection,
  ) {
    this.profileId = profileId;
    this.sessionId = sessionId;
    this.cdpEndpoint = cdpEndpoint;
    this.allowedOrigins = allowedOrigins;
    this.target = target;
    this.connection = connection;
    this.attachedAt = new Date().toISOString();
  }

  static async attach(args: Record<string, unknown>): Promise<BrowserSession> {
    const profileId = requireText(args.profile_id, 'profile_id', 200);
    const sessionId = requireText(args.session_id, 'session_id', 300);
    const cdpEndpoint = validateCdpEndpoint(args.cdp_endpoint);
    const allowedOrigins = normalizeAllowedOrigins(args.allowed_origins);
    const targets = await listCdpTargets(cdpEndpoint);
    const target = targets.find((candidate) => candidate.id === sessionId && candidate.type === 'page');
    if (!target) {
      const availableSessionIds = targets.filter((candidate) => candidate.type === 'page').map((candidate) => candidate.id);
      const availablePage = boundedCollection(availableSessionIds, {
        limit: 50,
        truncationReason: 'browser_target_diagnostic_limit',
      });
      throw new BrowserControlError('browser_session_not_found', 'The explicitly selected browser session was not found.', {
        profile_id: profileId,
        session_id: sessionId,
        available_session_ids: availablePage.items,
        available_session_ids_page: availablePage,
      });
    }
    const connection = await CdpConnection.connect(String(target.webSocketDebuggerUrl));
    const session = new BrowserSession(profileId, sessionId, cdpEndpoint, allowedOrigins, target, connection);
    try {
      await session.connection.call('Page.enable');
      await session.connection.call('DOM.enable');
      await session.connection.call('Accessibility.enable');
    } catch (error) {
      session.close();
      throw error;
    }
    return session;
  }

  info(): BrowserSessionInfo {
    return {
      profile_id: this.profileId,
      session_id: this.sessionId,
      cdp_endpoint: this.cdpEndpoint,
      allowed_origins: [...this.allowedOrigins],
      target: {
        id: this.target.id,
        type: this.target.type,
        title: safeText(this.target.title, 300),
        url: safeUrl(this.target.url),
      },
      attached_at: this.attachedAt,
      last_action: this.lastAction,
    };
  }

  async refreshTarget(): Promise<BrowserSessionInfo> {
    const targets = await listCdpTargets(this.cdpEndpoint);
    const target = targets.find((candidate) => candidate.id === this.sessionId && candidate.type === 'page');
    if (target) this.target = target;
    return this.info();
  }

  async navigate(urlValue: unknown): Promise<Record<string, unknown>> {
    const url = assertAllowedOrigin(requireText(urlValue, 'url', 4_000), this.allowedOrigins);
    const navigation = await this.connection.call('Page.navigate', { url });
    this.lastAction = 'navigate';
    return { url: safeUrl(url), frame_id: navigation.frameId ?? null, navigation_error: safeText(navigation.errorText, 400) || null };
  }

  async accessibilitySnapshot(args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const maxNodes = int(args.max_nodes, 200, 1, MAX_SNAPSHOT_NODES, 'max_nodes');
    const offset = int(args.offset, 0, 0, Number.MAX_SAFE_INTEGER, 'offset');
    const result = await this.connection.call('Accessibility.getFullAXTree', {});
    const page = boundedCollection(Array.isArray(result.nodes) ? result.nodes : [], {
      offset,
      limit: maxNodes,
      truncationReason: 'accessibility_node_page',
    });
    const nodes = page.items;
    return {
      schema: 'narada.browser_control.accessibility_snapshot.v1',
      session: this.info(),
      node_count: nodes.length,
      total_node_count: page.total_count,
      offset: page.offset,
      next_offset: page.next_offset,
      has_more: page.has_more,
      truncated: page.truncated,
      truncation_reason: page.truncation_reason,
      nodes: nodes.map((node: any) => ({
        node_id: safeText(node.nodeId, 100),
        ignored: Boolean(node.ignored),
        role: valueOf(node.role),
        name: valueOf(node.name),
        description: valueOf(node.description),
        value_available: Boolean(node.value && valueOf(node.value)),
        properties: Array.isArray(node.properties)
          ? node.properties
            .filter((property: any) => ['checked', 'disabled', 'expanded', 'focused', 'selected', 'level'].includes(String(property.name)))
            .map((property: any) => ({ name: safeText(property.name, 80), value: valueOf(property.value) }))
          : [],
      })),
    };
  }

  async screenshot(args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const format = args.format === undefined ? 'png' : args.format;
    if (format !== 'png' && format !== 'jpeg') throw new BrowserControlError('screenshot_format_invalid', 'format must be png or jpeg.');
    const quality = args.quality === undefined ? undefined : int(args.quality, 80, 0, 100, 'quality');
    const result = await this.connection.call('Page.captureScreenshot', {
      format,
      ...(quality === undefined ? {} : { quality }),
      fromSurface: true,
    }, 15_000);
    const data = typeof result.data === 'string' ? result.data : '';
    if (data.length === 0) throw new BrowserControlError('screenshot_empty', 'The browser returned an empty screenshot.');
    if (data.length > 10 * 1024 * 1024) throw new BrowserControlError('screenshot_too_large', 'The screenshot exceeds the bounded 10 MiB output limit.');
    return {
      schema: 'narada.browser_control.screenshot.v1',
      session: this.info(),
      content_type: format === 'png' ? 'image/png' : 'image/jpeg',
      encoding: 'base64',
      byte_length: Math.floor(data.length * 3 / 4),
      data_base64: data,
    };
  }

  async click(args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const targetSelector = selector(args.selector);
    const intent = (args.intent ?? 'verify') as BrowserActionIntent;
    if (!['verify', 'login', 'submit', 'destructive'].includes(intent)) throw new BrowserControlError('intent_invalid', 'intent must be verify, login, submit, or destructive.');
    requireConfirmedIntent(intent, args.confirmed);
    const node = await this.findNode(targetSelector);
    const description = await this.describeNode(node);
    if (isSensitiveField(targetSelector, description)) {
      throw new BrowserControlError('sensitive_field_refused', 'Sensitive authentication fields cannot be acted on through this surface.');
    }
    const center = await this.elementCenter(node);
    await this.connection.call('Input.dispatchMouseEvent', { type: 'mouseMoved', x: center.x, y: center.y });
    await this.connection.call('Input.dispatchMouseEvent', { type: 'mousePressed', x: center.x, y: center.y, button: 'left', clickCount: 1 });
    await this.connection.call('Input.dispatchMouseEvent', { type: 'mouseReleased', x: center.x, y: center.y, button: 'left', clickCount: 1 });
    this.lastAction = 'click';
    return { selector: targetSelector, intent, confirmed: intent === 'verify' ? false : true, clicked: true };
  }

  async fill(args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const targetSelector = selector(args.selector);
    const value = requireText(args.value, 'value', MAX_TEXT_LENGTH);
    const intent = (args.intent ?? 'verify') as BrowserActionIntent;
    if (!['verify', 'login', 'submit', 'destructive'].includes(intent)) throw new BrowserControlError('intent_invalid', 'intent must be verify, login, submit, or destructive.');
    requireConfirmedIntent(intent, args.confirmed);
    const node = await this.findNode(targetSelector);
    const description = await this.describeNode(node);
    const attrs = attributesRecord(description);
    if (isSensitiveField(targetSelector, description)) {
      throw new BrowserControlError('sensitive_field_refused', 'Password, token, secret, cookie, and authentication fields are never accepted.');
    }
    const nodeName = String(description.nodeName ?? '').toUpperCase();
    if (!['INPUT', 'TEXTAREA'].includes(nodeName) && !description.isContentEditable) {
      throw new BrowserControlError('fill_target_not_editable', 'fill is limited to input, textarea, and contenteditable elements.');
    }
    await this.connection.call('DOM.focus', { nodeId: node });
    await this.connection.call('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', code: 'KeyA', modifiers: 2 });
    await this.connection.call('Input.dispatchKeyEvent', { type: 'keyUp', key: 'a', code: 'KeyA', modifiers: 2 });
    await this.connection.call('Input.insertText', { text: value });
    this.lastAction = 'fill';
    return {
      selector: targetSelector,
      intent,
      confirmed: intent === 'verify' ? false : true,
      filled: true,
      value_length: value.length,
      value_sha256: createHash('sha256').update(value).digest('hex'),
      input_type: attrs.type ?? 'unknown',
    };
  }

  async wait(args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const timeoutMs = int(args.timeout_ms, 5_000, 1, MAX_WAIT_MS, 'timeout_ms');
    const targetSelector = args.selector === undefined ? null : selector(args.selector);
    const sleepMs = int(args.sleep_ms, targetSelector ? 0 : 250, 0, MAX_WAIT_MS, 'sleep_ms');
    const started = Date.now();
    if (sleepMs > 0) await boundedDelay(Math.min(sleepMs, timeoutMs));
    let found = targetSelector === null;
    while (!found && Date.now() - started < timeoutMs) {
      try {
        await this.findNode(targetSelector as string);
        found = true;
      } catch (error) {
        if (!(error instanceof BrowserControlError) || error.code !== 'selector_not_found') throw error;
        await boundedDelay(Math.min(100, timeoutMs));
      }
    }
    this.lastAction = 'wait';
    return { selector: targetSelector, found, elapsed_ms: Date.now() - started, timed_out: !found };
  }

  async assert(args: Record<string, unknown>): Promise<Record<string, unknown>> {
    const targetSelector = selector(args.selector);
    const node = await this.findNode(targetSelector);
    const outer = await this.connection.call('DOM.getOuterHTML', { nodeId: node });
    const html = redactMarkup(safeText(outer.outerHTML, 8_000));
    const expected = args.contains_text === undefined ? null : requireText(args.contains_text, 'contains_text', MAX_TEXT_LENGTH);
    const matched = expected === null ? true : html.includes(expected);
    return {
      schema: 'narada.browser_control.assertion.v1',
      session: this.info(),
      selector: targetSelector,
      matched,
      contains_text_requested: expected !== null,
      contains_text_length: expected?.length ?? 0,
    };
  }

  async status(): Promise<BrowserSessionInfo> {
    return this.refreshTarget();
  }

  close(): void {
    this.connection.close();
  }

  private async findNode(targetSelector: string): Promise<number> {
    const document = await this.connection.call('DOM.getDocument', { depth: 1, pierce: false });
    const rootNodeId = document.root?.nodeId;
    if (!Number.isInteger(rootNodeId)) throw new BrowserControlError('dom_document_missing', 'The browser did not return a DOM document.');
    const found = await this.connection.call('DOM.querySelector', { nodeId: rootNodeId, selector: targetSelector });
    if (!Number.isInteger(found.nodeId) || found.nodeId === 0) {
      throw new BrowserControlError('selector_not_found', 'The selector did not match an element.', { selector: targetSelector });
    }
    return found.nodeId;
  }

  private async describeNode(nodeId: number): Promise<NodeDescription> {
    const result = await this.connection.call('DOM.describeNode', { nodeId });
    return result.node ?? {};
  }

  private async elementCenter(nodeId: number): Promise<{ x: number; y: number }> {
    await this.connection.call('DOM.scrollIntoViewIfNeeded', { nodeId });
    const result = await this.connection.call('DOM.getBoxModel', { nodeId });
    return quadCenter(result.model?.content ?? result.model?.border);
  }
}